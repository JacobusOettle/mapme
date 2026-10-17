import Observation
import QuartzCore
import SceneKit
import SwiftUI
import UIKit

// The 3D scene (docs/design/explorer.md, 3D view): SceneKit in a UIView (deprecated on iOS 26,
// warnings accepted; works on iOS 17–26). The block (the map draped on the terrain, its sides),
// lines draped on it (routes, the track), billboards (POIs, waypoints, notes) and you, seen by
// an orbiting camera driven by our own gestures: one finger turns and tilts, two fingers move,
// pinch zooms, twist turns, double tap zooms in (two-finger tap out), with inertia. SceneKit's
// `allowsCameraControl` is not used: its pinch changes the field of view, it can fly under the
// terrain and it has no compass heading to read back.

/// Live readouts of the 3D camera for the SwiftUI chrome (the compass) and commands back.
@MainActor
@Observable
final class Terrain3DController {
    /// The camera's compass heading, degrees clockwise from north (the compass turns with it).
    private(set) var headingDegrees: Double = 0

    @ObservationIgnored weak var view: Terrain3DSceneUIView?

    init() {}

    func publish(headingDegrees h: Double) {
        if abs(MapAngle.normalizedDegrees(h - headingDegrees)) > 0.5 { headingDegrees = h }
    }

    /// Back to the start: north at 45° over the sheet (over you when you are on it).
    func resetCamera() {
        view?.resetCamera()
    }

    /// Turn to face north, keeping the rest.
    func faceNorth() {
        view?.faceNorth()
    }
}

/// What the scene shows, from the screen.
struct Terrain3DSceneInputs {
    var model: Terrain3DModel?
    var exaggeration: Double
    var you: LatLon?
    var heading: Double?
    /// The recorded track (empty: not shown).
    var track: [TrackPoint]
    var items: Terrain3DItems
    var itemsRevision: Int
    var maxFrameRate: Int
    /// 4× MSAA (off with the battery saver).
    var antialiasing: Bool
}

/// SwiftUI wrapper of the scene.
struct Terrain3DSceneView: UIViewRepresentable {
    let inputs: Terrain3DSceneInputs
    let controller: Terrain3DController
    /// "3D view of Grootkloof".
    let accessibilityName: String

    func makeUIView(context: Context) -> Terrain3DSceneUIView {
        Terrain3DSceneUIView(frame: .zero)
    }

    func updateUIView(_ uiView: Terrain3DSceneUIView, context: Context) {
        controller.view = uiView
        uiView.controller = controller
        uiView.setAccessibilityName(accessibilityName)
        uiView.apply(inputs)
    }

    static func dismantleUIView(_ uiView: Terrain3DSceneUIView, coordinator: Coordinator) {
        uiView.tearDown()
    }
}

/// The SceneKit view, its scene graph and camera rig.
@MainActor
final class Terrain3DSceneUIView: UIView, UIGestureRecognizerDelegate {
    weak var controller: Terrain3DController?

    /// The terrain's hit-test category (double tap zooms to where it lands).
    private static let terrainCategory = 2
    /// Field of view across the screen's shorter side.
    private static let fovDegrees = 55.0
    /// Drawing order: terrain (0), lines, billboards, you.
    private static let lineOrder = 600
    private static let markerOrder = 2000
    private static let youOrder = 4000

    private let scnView = SCNView(frame: .zero, options: nil)
    private let scene = SCNScene()
    private let cameraNode = SCNNode()
    private let camera = SCNCamera()
    /// The block, scaled in y by the exaggeration.
    private let terrainNode = SCNNode()
    private let linesNode = SCNNode()
    private let markersNode = SCNNode()
    private let youNode = SCNNode()
    private let youHeading = SCNNode()

    private var model: Terrain3DModel?
    private var orbit = Terrain3DOrbit(target: SIMD3(0, 0, 0), yaw: 0, pitch: Terrain3DOrbit.defaultPitch, distance: 10_000)
    private var exaggeration = Terrain3DLimits.defaultExaggeration
    private var maxDistance = 100_000.0
    private var extent = 10_000.0

    // Overlays.
    private var sprites: [Terrain3DSprite] = []
    private var itemsRevision = -1
    private var items = Terrain3DItems()
    private var routePieces: [(line: Terrain3DLine, pieces: [[Terrain3DDrapedPoint]])] = []
    private var trackPieces: [[Terrain3DDrapedPoint]] = []
    private var trackCount = -1
    private var trackFirst: Date?
    private var trackBuiltAt = Date.distantPast
    private var trackVersion = 0
    private var ribbonKey: RibbonKey?
    private var you: LatLon?
    private var youLocal: SIMD3<Double>?
    private var heading: Double?

    // Gestures, inertia, animation.
    private let oneFinger = UIPanGestureRecognizer()
    private let twoFingers = UIPanGestureRecognizer()
    private let pinch = UIPinchGestureRecognizer()
    private let twist = UIRotationGestureRecognizer()
    private let doubleTap = UITapGestureRecognizer()
    private let twoFingerTap = UITapGestureRecognizer()
    private var displayLink: CADisplayLink?
    private var lastTimestamp: CFTimeInterval = 0
    private var animation: (from: Terrain3DOrbit, to: Terrain3DOrbit, start: CFTimeInterval, duration: Double)?
    /// Yaw and pitch, radians per second.
    private var orbitVelocity = SIMD2<Double>(0, 0)
    /// The target on the ground, metres per second (x, z).
    private var moveVelocity = SIMD2<Double>(0, 0)
    private var pinchStartDistance = 0.0
    private var twistStartYaw = 0.0
    private var maxFrameRate = 60
    private var laidOutSize = CGSize.zero

    private struct RibbonKey: Equatable {
        var bucket: Int
        var exaggeration: Double
        var routes: Int
        var track: Int
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = Theme.uiBackground
        isMultipleTouchEnabled = true
        scnView.frame = bounds
        scnView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        scnView.scene = scene
        scnView.backgroundColor = Theme.uiBackground
        scnView.allowsCameraControl = false
        scnView.rendersContinuously = false
        scnView.antialiasingMode = .multisampling4X
        scnView.preferredFramesPerSecond = maxFrameRate
        scnView.isMultipleTouchEnabled = true
        scnView.isAccessibilityElement = true
        scnView.accessibilityIdentifier = "terrain3DScene"
        scnView.accessibilityLabel = "3D view"
        scnView.accessibilityHint = "Drag to turn the view, pinch to zoom, drag with two fingers to move"
        scnView.accessibilityTraits = .allowsDirectInteraction
        addSubview(scnView)
        setUpScene()
        setUpGestures()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    deinit {
        displayLink?.invalidate()
    }

    override func willMove(toWindow newWindow: UIWindow?) {
        super.willMove(toWindow: newWindow)
        if newWindow == nil { stopLink() }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        // The field of view spans the shorter side (portrait: across, landscape: up and down).
        let direction: SCNCameraProjectionDirection = bounds.width < bounds.height ? .horizontal : .vertical
        if camera.projectionDirection != direction { camera.projectionDirection = direction }
        // Billboards and line widths are sized in points of this view.
        if bounds.size != laidOutSize {
            laidOutSize = bounds.size
            ribbonKey = nil
            applyOrbit()
        }
    }

    func setAccessibilityName(_ name: String) {
        if scnView.accessibilityLabel != name { scnView.accessibilityLabel = name }
    }

    /// Frees the scene's GPU resources at once when the screen closes.
    func tearDown() {
        stopLink()
        controller = nil
        scnView.scene = nil
    }

    // MARK: - Scene

    private func setUpScene() {
        scene.background.contents = Self.skyImage()
        scene.fogColor = Self.horizon
        scene.fogDensityExponent = 1
        camera.fieldOfView = CGFloat(Self.fovDegrees)
        // Set from the orbit (`applyOrbit`): near and far planes in proportion to the distance keep
        // the depth precision that lines draped a few metres above the terrain need.
        camera.automaticallyAdjustsZRange = false
        cameraNode.camera = camera
        scene.rootNode.addChildNode(cameraNode)
        scnView.pointOfView = cameraNode

        // Soft light from everywhere plus the hillshade's sun (north-west, 45° up): the sheet,
        // which has no hillshade of its own, reads as relief.
        let ambient = SCNLight()
        ambient.type = .ambient
        ambient.intensity = 520
        ambient.color = UIColor.white
        let ambientNode = SCNNode()
        ambientNode.light = ambient
        scene.rootNode.addChildNode(ambientNode)
        let sun = SCNLight()
        sun.type = .directional
        sun.intensity = 680
        sun.color = UIColor.white
        let sunNode = SCNNode()
        sunNode.light = sun
        sunNode.simdLook(at: SIMD3<Float>(0.5, -0.7071, 0.5), up: SIMD3<Float>(0, 1, 0), localFront: SIMD3<Float>(0, 0, -1))
        scene.rootNode.addChildNode(sunNode)

        for node in [terrainNode, linesNode, markersNode, youNode] {
            scene.rootNode.addChildNode(node)
        }
        buildYou()
        youNode.isHidden = true
    }

    /// Night sky to near-black ground, as a spherical backdrop (2:1): the horizon sits mid-image.
    private static let zenith = UIColor(red: 0.07, green: 0.08, blue: 0.11, alpha: 1)
    private static let horizon = UIColor(red: 0.15, green: 0.16, blue: 0.19, alpha: 1)

    private static func skyImage() -> UIImage {
        let size = CGSize(width: 128, height: 64)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        return UIGraphicsImageRenderer(size: size, format: format).image { ctx in
            let colors = [zenith.cgColor, horizon.cgColor, Theme.uiBackground.cgColor] as CFArray
            if let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: colors, locations: [0, 0.5, 0.75]) {
                ctx.cgContext.drawLinearGradient(gradient, start: .zero, end: CGPoint(x: 0, y: size.height),
                                                 options: [.drawsAfterEndLocation])
            }
        }
    }

    /// You: a red ball on a white pole with a cone pointing where you face, always on top.
    private func buildYou() {
        let red = Theme.uiUserRed
        let ballRadius: CGFloat = 7
        let poleHeight: CGFloat = 24
        let sphere = SCNSphere(radius: ballRadius)
        sphere.segmentCount = 24
        let ballMaterial = Self.overlayMaterial(color: red, lit: true)
        sphere.firstMaterial = ballMaterial
        let ball = SCNNode(geometry: sphere)
        ball.position = SCNVector3(0, Float(poleHeight), 0)
        let pole = SCNCylinder(radius: 1.3, height: poleHeight)
        pole.firstMaterial = Self.overlayMaterial(color: .white, lit: false)
        let poleNode = SCNNode(geometry: pole)
        poleNode.position = SCNVector3(0, Float(poleHeight / 2), 0)
        let cone = SCNCone(topRadius: 0, bottomRadius: 6, height: 16)
        cone.firstMaterial = Self.overlayMaterial(color: red.withAlphaComponent(0.85), lit: false)
        let coneNode = SCNNode(geometry: cone)
        // The cone's apex (+y) turned to point north (−z), in front of the ball.
        coneNode.eulerAngles = SCNVector3(-Float.pi / 2, 0, 0)
        coneNode.position = SCNVector3(0, 0, -Float(ballRadius) - 10)
        youHeading.position = SCNVector3(0, Float(poleHeight), 0)
        youHeading.addChildNode(coneNode)
        for node in [poleNode, ball, coneNode] {
            node.renderingOrder = Self.youOrder
        }
        youNode.addChildNode(poleNode)
        youNode.addChildNode(ball)
        youNode.addChildNode(youHeading)
    }

    private static func overlayMaterial(color: UIColor, lit: Bool) -> SCNMaterial {
        let m = SCNMaterial()
        m.diffuse.contents = color
        m.lightingModel = lit ? .lambert : .constant
        if lit {
            m.emission.contents = color
            m.emission.intensity = 0.35
        }
        // Always visible (you, behind a ridge, still show).
        m.readsFromDepthBuffer = false
        m.writesToDepthBuffer = false
        return m
    }

    /// A model built: the block, then everything on it, and the camera at its start.
    private func load(_ model: Terrain3DModel) {
        self.model = model
        stopMotion()
        terrainNode.childNodes.forEach { $0.removeFromParentNode() }

        let material = SCNMaterial()
        material.diffuse.contents = model.texture
        material.diffuse.mipFilter = .linear
        material.diffuse.minificationFilter = .linear
        material.diffuse.magnificationFilter = .linear
        material.diffuse.wrapS = .clamp
        material.diffuse.wrapT = .clamp
        material.diffuse.maxAnisotropy = 8
        material.lightingModel = .lambert
        material.isDoubleSided = true
        let mesh = model.mesh
        let surface = Self.geometry(positions: mesh.positions, normals: mesh.normals, uvs: mesh.uvs, colors: nil,
                                    indices: mesh.indices)
        surface.firstMaterial = material
        if let coarse = model.coarseMesh, coarse.vertexCount < mesh.vertexCount {
            let lod = Self.geometry(positions: coarse.positions, normals: coarse.normals, uvs: coarse.uvs, colors: nil,
                                    indices: coarse.indices)
            lod.firstMaterial = material
            surface.levelsOfDetail = [SCNLevelOfDetail(geometry: lod, screenSpaceRadius: CGFloat(Terrain3DLimits.coarseScreenRadius))]
        }
        let surfaceNode = SCNNode(geometry: surface)
        surfaceNode.categoryBitMask = Self.terrainCategory
        terrainNode.addChildNode(surfaceNode)

        if !mesh.skirtIndices.isEmpty {
            // The block's sides: the off-map paper's shade at the top, darker below.
            let top = Terrain3DLine.rgba(model.dark ? UIColor(red: 0.16, green: 0.16, blue: 0.17, alpha: 1)
                : UIColor(red: 0.62, green: 0.58, blue: 0.52, alpha: 1))
            let bottom = Terrain3DLine.rgba(model.dark ? UIColor(red: 0.05, green: 0.05, blue: 0.06, alpha: 1)
                : UIColor(red: 0.30, green: 0.27, blue: 0.23, alpha: 1))
            var colors: [Float] = []
            colors.reserveCapacity(mesh.skirtShade.count * 4)
            for t in mesh.skirtShade {
                // Vertex colours are linear light (unlike material colours, nothing converts them):
                // sRGB values as they are came out about three times too bright.
                let c = Terrain3DLine.linear(top + (bottom - top) * t)
                colors.append(contentsOf: [c.x, c.y, c.z, 1])
            }
            let sides = Self.geometry(positions: mesh.skirtPositions, normals: nil, uvs: nil, colors: colors,
                                      indices: mesh.skirtIndices)
            let sideMaterial = SCNMaterial()
            sideMaterial.diffuse.contents = UIColor.white
            sideMaterial.lightingModel = .constant
            sideMaterial.isDoubleSided = true
            sides.firstMaterial = sideMaterial
            terrainNode.addChildNode(SCNNode(geometry: sides))
        }
        terrainNode.scale = SCNVector3(1, Float(exaggeration), 1)

        extent = model.extentM
        maxDistance = extent * 3
        orbit = homeOrbit()
        rebuildMarkers()
        drapeRoutes()
        ribbonKey = nil
        updateYou()
        applyOrbit()
    }

    /// Flat arrays → a SceneKit geometry of triangles.
    private static func geometry(positions: [Float], normals: [Float]?, uvs: [Float]?, colors: [Float]?,
                                 indices: [UInt32]) -> SCNGeometry {
        let count = positions.count / 3
        var sources = [source(positions, semantic: .vertex, components: 3, count: count)]
        if let normals { sources.append(source(normals, semantic: .normal, components: 3, count: count)) }
        if let uvs { sources.append(source(uvs, semantic: .texcoord, components: 2, count: count)) }
        if let colors { sources.append(source(colors, semantic: .color, components: 4, count: count)) }
        let indexData = indices.withUnsafeBufferPointer { Data(buffer: $0) }
        let element = SCNGeometryElement(data: indexData, primitiveType: .triangles, primitiveCount: indices.count / 3,
                                         bytesPerIndex: MemoryLayout<UInt32>.size)
        return SCNGeometry(sources: sources, elements: [element])
    }

    private static func source(_ values: [Float], semantic: SCNGeometrySource.Semantic, components: Int,
                               count: Int) -> SCNGeometrySource {
        let data = values.withUnsafeBufferPointer { Data(buffer: $0) }
        return SCNGeometrySource(data: data, semantic: semantic, vectorCount: count, usesFloatComponents: true,
                                 componentsPerVector: components, bytesPerComponent: MemoryLayout<Float>.size,
                                 dataOffset: 0, dataStride: MemoryLayout<Float>.size * components)
    }

    // MARK: - Inputs

    func apply(_ inputs: Terrain3DSceneInputs) {
        let fps = max(15, min(inputs.maxFrameRate, 60))
        if fps != maxFrameRate {
            maxFrameRate = fps
            scnView.preferredFramesPerSecond = fps
        }
        let aa: SCNAntialiasingMode = inputs.antialiasing ? .multisampling4X : .none
        if scnView.antialiasingMode != aa { scnView.antialiasingMode = aa }
        you = inputs.you
        heading = inputs.heading
        items = inputs.items
        let wanted = min(max(inputs.exaggeration.isFinite ? inputs.exaggeration : Terrain3DLimits.defaultExaggeration,
                             Terrain3DLimits.exaggerationRange.lowerBound), Terrain3DLimits.exaggerationRange.upperBound)

        if let next = inputs.model, next.id != model?.id {
            exaggeration = wanted
            itemsRevision = inputs.itemsRevision
            trackCount = -1
            load(next)
            updateTrack(inputs.track, force: true)
            return
        }
        guard model != nil else { return }
        if wanted != exaggeration { setExaggeration(wanted) }
        if inputs.itemsRevision != itemsRevision {
            itemsRevision = inputs.itemsRevision
            rebuildMarkers()
            drapeRoutes()
            rebuildRibbons()
            updateSprites()
        }
        updateTrack(inputs.track, force: false)
        updateYou()
    }

    private func setExaggeration(_ value: Double) {
        guard let model else { return }
        exaggeration = value
        terrainNode.scale = SCNVector3(1, Float(value), 1)
        // The target stays on the ground.
        let g = model.surface.gridPoint(local: SIMD2(orbit.target.x, orbit.target.z))
        if let h = model.surface.height(at: clampedGrid(g)) { orbit.target.y = h * value }
        for sprite in sprites {
            sprite.anchor.simdPosition = SIMD3<Float>(Float(sprite.local.x), Float(sprite.local.y * value), Float(sprite.local.z))
        }
        updateYou()
        applyOrbit()
    }

    // MARK: - Billboards

    /// Billboards for the markers on the block: the disc at the ground, the label above it.
    private func rebuildMarkers() {
        markersNode.childNodes.forEach { $0.removeFromParentNode() }
        sprites = []
        guard let model else { return }
        for marker in items.markers {
            let g = model.area.gridPoint(for: marker.position)
            guard let p = model.surface.point(at: g) else { continue }
            let image = Terrain3DArt.marker(for: marker.style)
            let anchor = SCNNode()
            let billboard = SCNBillboardConstraint()
            billboard.freeAxes = .all
            anchor.constraints = [billboard]
            anchor.simdPosition = SIMD3<Float>(Float(p.x), Float(p.y * exaggeration), Float(p.z))
            let disc = Self.spriteNode(image)
            // The disc's lower edge (its image has `markerPadding` around it) on the ground.
            let discCentre = image.size.height / 2 - OverlayArt.markerPadding
            disc.position = SCNVector3(0, Float(discCentre), 0)
            anchor.addChildNode(disc)
            var label: SCNNode?
            var labelSize: CGSize?
            if !marker.name.isEmpty {
                let text = OverlayArt.haloText(marker.name, color: .white, halo: Self.labelHalo, fontSize: 13, weight: .semibold)
                let node = Self.spriteNode(text)
                let lift = discCentre + OverlayArt.markerSize / 2 + Terrain3DDeclutter.labelGap + text.size.height / 2
                node.position = SCNVector3(0, Float(lift), 0)
                node.isHidden = true
                anchor.addChildNode(node)
                label = node
                labelSize = text.size
            }
            anchor.opacity = marker.isForeign ? 0.8 : 1
            markersNode.addChildNode(anchor)
            sprites.append(Terrain3DSprite(marker: marker, anchor: anchor, disc: disc, label: label, local: p,
                                           discLift: discCentre, labelSize: labelSize))
        }
    }

    private static let labelHalo = UIColor(red: 0.05, green: 0.05, blue: 0.06, alpha: 0.88)

    /// A camera-facing picture, sized in points (the anchor's scale turns points into metres).
    private static func spriteNode(_ image: UIImage) -> SCNNode {
        let plane = SCNPlane(width: image.size.width, height: image.size.height)
        let m = SCNMaterial()
        m.diffuse.contents = image
        m.lightingModel = .constant
        m.isDoubleSided = true
        // Drawn over the terrain, in the order `updateSprites` gives (far ones first).
        m.readsFromDepthBuffer = false
        m.writesToDepthBuffer = false
        plane.firstMaterial = m
        return SCNNode(geometry: plane)
    }

    /// Constant size on screen, fading with distance, decluttered (no disc or label covering a
    /// more important one), far ones drawn first.
    private func updateSprites() {
        let eye = orbit.position
        let forward = orbit.forward
        let viewSize = bounds.size
        let viewPoints = Double(max(min(viewSize.width, viewSize.height), 1))
        // Labels within range of the camera.
        let range = max(Terrain3DLimits.labelNearM, orbit.distance * 1.4)
        var ranked: [(sprite: Terrain3DSprite, distance: Double)] = []
        ranked.reserveCapacity(sprites.count)
        var candidates: [Terrain3DDeclutter.Candidate] = []
        candidates.reserveCapacity(sprites.count)
        for sprite in sprites {
            let p = SIMD3<Double>(sprite.local.x, sprite.local.y * exaggeration, sprite.local.z)
            let d = Self.length(p - eye)
            let mpp = Float(Terrain3DOrbit.metersPerPoint(distance: d, fovDegrees: Self.fovDegrees, viewPoints: viewPoints))
            sprite.anchor.scale = SCNVector3(mpp, mpp, mpp)
            // Full strength up to the orbit distance, half at three times it.
            let t = min(max((d - orbit.distance) / (2 * orbit.distance), 0), 1)
            let fade = 1 - 0.5 * t
            sprite.anchor.opacity = CGFloat(fade * (sprite.marker.isForeign ? 0.8 : 1))
            ranked.append((sprite, d))
            let depth = ((p - eye) * forward).sum()
            let scale = depth > 1e-6 ? CGFloat(d / depth) : 1
            var disc: CGPoint?
            if let anchor = orbit.screenPoint(of: p, fovDegrees: Self.fovDegrees, size: viewSize) {
                disc = CGPoint(x: anchor.x, y: anchor.y - sprite.discLift * scale)
            }
            candidates.append(Terrain3DDeclutter.Candidate(disc: disc, label: d <= range ? sprite.labelSize : nil,
                                                           priority: sprite.marker.priority, distance: d,
                                                           wasShown: sprite.shown, scale: scale))
        }
        // Far first.
        let order = ranked.sorted { $0.distance > $1.distance }
        for (k, entry) in order.enumerated() {
            entry.sprite.disc.renderingOrder = Self.markerOrder + 2 * k
            entry.sprite.label?.renderingOrder = Self.markerOrder + 2 * k + 1
        }
        let placements = Terrain3DDeclutter.place(candidates, markerSize: OverlayArt.markerSize, viewSize: viewSize,
                                                  maxLabels: Terrain3DLimits.maxLabels)
        for (sprite, placement) in zip(sprites, placements) {
            sprite.shown = placement.disc
            if sprite.disc.isHidden == placement.disc { sprite.disc.isHidden = !placement.disc }
            let labelHidden = !placement.label
            if let label = sprite.label, label.isHidden != labelHidden { label.isHidden = labelHidden }
        }
        if let youLocal, !youNode.isHidden {
            let p = SIMD3<Double>(youLocal.x, youLocal.y * exaggeration, youLocal.z)
            let mpp = Float(Terrain3DOrbit.metersPerPoint(distance: Self.length(p - eye), fovDegrees: Self.fovDegrees,
                                                          viewPoints: viewPoints))
            youNode.scale = SCNVector3(mpp, mpp, mpp)
        }
    }

    private static func length(_ v: SIMD3<Double>) -> Double { (v.x * v.x + v.y * v.y + v.z * v.z).squareRoot() }

    /// You, on the block when you are on it.
    private func updateYou() {
        guard let model, let you else {
            youNode.isHidden = true
            youLocal = nil
            return
        }
        let g = model.area.gridPoint(for: you)
        guard let p = model.surface.point(at: g) else {
            youNode.isHidden = true
            youLocal = nil
            return
        }
        let moved = youLocal == nil || youNode.isHidden
        youLocal = p
        youNode.isHidden = false
        youNode.simdPosition = SIMD3<Float>(Float(p.x), Float(p.y * exaggeration), Float(p.z))
        if let heading, heading.isFinite {
            youHeading.isHidden = false
            youHeading.eulerAngles = SCNVector3(0, Float(-heading * .pi / 180), 0)
        } else {
            youHeading.isHidden = true
        }
        if moved { updateSprites() }
    }

    // MARK: - Lines

    /// Routes laid on the block (once per change of the routes).
    private func drapeRoutes() {
        routePieces = []
        guard let model else { return }
        for line in items.routes {
            let pieces = drape(line.points, model: model)
            if !pieces.isEmpty { routePieces.append((line, pieces)) }
        }
        ribbonKey = nil
    }

    /// The track laid on the block again when it has grown (at most every 2 s while walking).
    private func updateTrack(_ track: [TrackPoint], force: Bool) {
        guard let model else { return }
        let first = track.first?.timestamp
        guard force || track.count != trackCount || first != trackFirst else { return }
        let now = Date()
        // A new or cleared track redraws at once; a growing one in steps.
        let growing = first == trackFirst && track.count > trackCount && trackCount > 0
        guard force || !growing || now.timeIntervalSince(trackBuiltAt) >= 2 else { return }
        trackCount = track.count
        trackFirst = first
        trackBuiltAt = now
        trackPieces = drape(track.map(\.latLon), model: model)
        trackVersion += 1
        rebuildRibbons()
    }

    /// A line's points on the block: map pixels through the inverse (with the warp), simplified to
    /// a fifth of a cell, then split along the triangles.
    private func drape(_ points: [LatLon], model: Terrain3DModel) -> [[Terrain3DDrapedPoint]] {
        guard points.count >= 2 else { return [] }
        let grid = points.map { model.area.gridPoint(for: $0) }
        let simple = Terrain3DLines.simplify(grid, tolerance: 0.2)
        return model.surface.drape(simple)
    }

    /// Ribbons for the lines at the width and lift of the current zoom (in √2 steps) and the
    /// exaggeration.
    private func rebuildRibbons() {
        guard let model else { return }
        let bucket = Int((log2(max(orbit.distance, 1)) * 2).rounded())
        let key = RibbonKey(bucket: bucket, exaggeration: exaggeration, routes: itemsRevision, track: trackVersion)
        guard key != ribbonKey else { return }
        ribbonKey = key
        linesNode.childNodes.forEach { $0.removeFromParentNode() }
        let distance = pow(2, Double(bucket) / 2)
        let viewPoints = Double(max(min(bounds.width, bounds.height), 1))
        let mpp = Terrain3DOrbit.metersPerPoint(distance: distance, fovDegrees: Self.fovDegrees, viewPoints: viewPoints)
        let lift = Terrain3DLines.lift(cameraDistance: distance)
        for (line, pieces) in routePieces {
            for piece in pieces {
                if let node = ribbonNode(piece, surface: model.surface, width: line.width * mpp, lift: lift,
                                         color: line.color, order: Self.lineOrder) {
                    linesNode.addChildNode(node)
                }
            }
        }
        // The track over routes (a little higher, drawn later).
        let pink = Terrain3DLine.rgba(Theme.uiTrack)
        for piece in trackPieces {
            if let node = ribbonNode(piece, surface: model.surface, width: 3.5 * mpp, lift: lift * 1.15, color: pink,
                                     order: Self.lineOrder + 1) {
                linesNode.addChildNode(node)
            }
        }
    }

    private func ribbonNode(_ piece: [Terrain3DDrapedPoint], surface: Terrain3DSurface, width: Double, lift: Double,
                            color: SIMD4<Float>, order: Int) -> SCNNode? {
        let ribbon = Terrain3DLines.ribbon(piece, surface: surface, width: width, lift: lift, exaggeration: exaggeration)
        guard !ribbon.indices.isEmpty else { return nil }
        let geometry = Self.geometry(positions: ribbon.positions, normals: nil, uvs: nil, colors: nil, indices: ribbon.indices)
        let m = SCNMaterial()
        m.diffuse.contents = UIColor(red: CGFloat(color.x), green: CGFloat(color.y), blue: CGFloat(color.z),
                                     alpha: CGFloat(color.w))
        m.lightingModel = .constant
        m.isDoubleSided = true
        geometry.firstMaterial = m
        let node = SCNNode(geometry: geometry)
        node.renderingOrder = order
        return node
    }

    // MARK: - Camera

    /// North at 45° over the sheet, or over you when you are on it, the sheet filling the view.
    private func homeOrbit() -> Terrain3DOrbit {
        guard let model else { return orbit }
        let area = model.area
        var g = area.gridPoint(pixel: SIMD2(area.sheetWidth / 2, area.sheetHeight / 2))
        if let you, area.sheetContains(you) { g = area.gridPoint(for: you) }
        var target = SIMD3<Double>(0, 0, 0)
        if let p = model.surface.point(at: g) { target = SIMD3(p.x, p.y * exaggeration, p.z) }
        let sheetExtent = max(area.sheetGroundSize.x, area.sheetGroundSize.y)
        let distance = Terrain3DOrbit.fitDistance(extent: sheetExtent, fovDegrees: Self.fovDegrees)
        return Terrain3DOrbit(target: target, yaw: 0, pitch: Terrain3DOrbit.defaultPitch, distance: distance)
            .clamped(maxDistance: maxDistance)
    }

    func resetCamera() {
        animate(to: homeOrbit(), duration: 0.6)
    }

    func faceNorth() {
        var to = orbit
        to.yaw = 0
        animate(to: to, duration: 0.45)
    }

    private func clampedGrid(_ g: SIMD2<Double>) -> SIMD2<Double> {
        guard let surface = model?.surface else { return g }
        return SIMD2(min(max(g.x.isFinite ? g.x : 0, 0), Double(surface.cols - 1)),
                     min(max(g.y.isFinite ? g.y : 0, 0), Double(surface.rows - 1)))
    }

    /// The target on the ground: on the block, at the terrain's height there.
    private func settleTarget() {
        guard let surface = model?.surface else { return }
        let g = surface.gridPoint(local: SIMD2(orbit.target.x, orbit.target.z))
        let inside = g.x >= 0 && g.y >= 0 && g.x <= Double(surface.cols - 1) && g.y <= Double(surface.rows - 1)
        if inside, let h = surface.height(at: g) {
            orbit.target.y = h * exaggeration
        } else if let p = surface.point(at: clampedGrid(g)) {
            orbit.target = SIMD3(p.x, p.y * exaggeration, p.z)
            moveVelocity = SIMD2(0, 0)
        }
    }

    /// Never under the terrain: tilt up until the camera is 40 m above what is below it.
    private func keepAboveTerrain() {
        guard let surface = model?.surface else { return }
        for _ in 0..<40 {
            let p = orbit.position
            let g = surface.gridPoint(local: SIMD2(p.x, p.z))
            guard let h = surface.height(at: g) else { return }
            if p.y >= h * exaggeration + 40 || orbit.pitch >= Terrain3DOrbit.maxPitch { return }
            orbit.pitch = min(orbit.pitch + 2 * Double.pi / 180, Terrain3DOrbit.maxPitch)
        }
    }

    private func applyOrbit() {
        orbit = orbit.clamped(maxDistance: maxDistance)
        keepAboveTerrain()
        let p = orbit.position
        let t = orbit.target
        cameraNode.simdPosition = SIMD3<Float>(Float(p.x), Float(p.y), Float(p.z))
        cameraNode.simdLook(at: SIMD3<Float>(Float(t.x), Float(t.y), Float(t.z)), up: SIMD3<Float>(0, 1, 0),
                            localFront: SIMD3<Float>(0, 0, -1))
        camera.zNear = max(2, orbit.distance * 0.02)
        camera.zFar = orbit.distance * 2 + extent * 3
        // Haze beyond the block's far side, whatever the zoom.
        scene.fogStartDistance = CGFloat(orbit.distance + extent * 0.6)
        scene.fogEndDistance = CGFloat(orbit.distance + extent * 2.5)
        updateSprites()
        rebuildRibbons()
        controller?.publish(headingDegrees: orbit.headingDegrees)
    }

    private func animate(to target: Terrain3DOrbit, duration: Double) {
        stopMotion()
        let to = target.clamped(maxDistance: maxDistance)
        guard window != nil else {
            orbit = to
            applyOrbit()
            return
        }
        animation = (orbit, to, CACurrentMediaTime(), duration)
        startLink()
    }

    private func stopMotion() {
        animation = nil
        orbitVelocity = SIMD2(0, 0)
        moveVelocity = SIMD2(0, 0)
    }

    private func startLink() {
        guard displayLink == nil, window != nil else { return }
        let link = CADisplayLink(target: Terrain3DLinkProxy(self), selector: #selector(Terrain3DLinkProxy.tick(_:)))
        let fps = Float(maxFrameRate)
        link.preferredFrameRateRange = CAFrameRateRange(minimum: min(30, fps), maximum: fps, preferred: fps)
        link.add(to: .main, forMode: .common)
        displayLink = link
        lastTimestamp = 0
    }

    private func stopLink() {
        displayLink?.invalidate()
        displayLink = nil
    }

    /// One frame of an animation or of inertia; the link stops when nothing moves any more.
    fileprivate func step(_ link: CADisplayLink) {
        let now = link.timestamp
        let dt = lastTimestamp == 0 ? 1.0 / 60 : min(max(now - lastTimestamp, 1.0 / 240), 1.0 / 15)
        lastTimestamp = now
        var active = false
        if let a = animation {
            let t = min(1, max(0, (CACurrentMediaTime() - a.start) / max(a.duration, 0.01)))
            // Ease in and out (quadratic).
            let rest: Double = 2 - 2 * t
            let eased: Double = t < 0.5 ? 2 * t * t : 1 - rest * rest / 2
            orbit = a.from.interpolated(to: a.to, eased)
            if t >= 1 {
                animation = nil
            } else {
                active = true
            }
        } else {
            let decay = exp(-dt / 0.32)
            if abs(orbitVelocity.x) + abs(orbitVelocity.y) > 0.02 {
                orbit.yaw += orbitVelocity.x * dt
                orbit.pitch += orbitVelocity.y * dt
                orbitVelocity *= decay
                active = true
            } else {
                orbitVelocity = SIMD2(0, 0)
            }
            let speed = (moveVelocity.x * moveVelocity.x + moveVelocity.y * moveVelocity.y).squareRoot()
            if speed > orbit.distance * 0.01 {
                orbit.target.x += moveVelocity.x * dt
                orbit.target.z += moveVelocity.y * dt
                moveVelocity *= decay
                settleTarget()
                active = true
            } else {
                moveVelocity = SIMD2(0, 0)
            }
        }
        applyOrbit()
        if !active { stopLink() }
    }

    // MARK: - Gestures

    private func setUpGestures() {
        oneFinger.addTarget(self, action: #selector(handleTurn(_:)))
        oneFinger.maximumNumberOfTouches = 1
        twoFingers.addTarget(self, action: #selector(handleMove(_:)))
        twoFingers.minimumNumberOfTouches = 2
        twoFingers.maximumNumberOfTouches = 2
        pinch.addTarget(self, action: #selector(handlePinch(_:)))
        twist.addTarget(self, action: #selector(handleTwist(_:)))
        doubleTap.addTarget(self, action: #selector(handleDoubleTap(_:)))
        doubleTap.numberOfTapsRequired = 2
        twoFingerTap.addTarget(self, action: #selector(handleTwoFingerTap(_:)))
        twoFingerTap.numberOfTouchesRequired = 2
        for g in [oneFinger, twoFingers, pinch, twist, doubleTap, twoFingerTap] as [UIGestureRecognizer] {
            g.delegate = self
            scnView.addGestureRecognizer(g)
        }
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer) -> Bool {
        let together: [UIGestureRecognizer] = [twoFingers, pinch, twist]
        return together.contains(gestureRecognizer) && together.contains(otherGestureRecognizer)
    }

    /// Radians per point of a one-finger drag: across the screen turns half way round.
    private var turnRate: Double { Double.pi / Double(max(bounds.width, 1)) }

    /// One finger: turn (sideways) and tilt (up and down) around the target, like a turntable;
    /// the scene follows the finger.
    @objc private func handleTurn(_ g: UIPanGestureRecognizer) {
        switch g.state {
        case .began:
            stopMotion()
        case .changed:
            let t = g.translation(in: self)
            g.setTranslation(.zero, in: self)
            orbit.yaw += Double(t.x) * turnRate
            orbit.pitch += Double(t.y) * turnRate * 0.6
            applyOrbit()
        case .ended:
            let v = g.velocity(in: self)
            orbitVelocity = SIMD2(Double(v.x) * turnRate, Double(v.y) * turnRate * 0.6)
            if abs(orbitVelocity.x) + abs(orbitVelocity.y) > 0.3 { startLink() } else { orbitVelocity = SIMD2(0, 0) }
        default:
            break
        }
    }

    /// Ground metres per point at the target, along the screen's (x) and into it (y).
    private var moveScale: SIMD2<Double> {
        let mpp = Terrain3DOrbit.metersPerPoint(distance: orbit.distance, fovDegrees: Self.fovDegrees,
                                                viewPoints: Double(max(min(bounds.width, bounds.height), 1)))
        return SIMD2(mpp, mpp / max(sin(orbit.pitch), 0.3))
    }

    /// Two fingers: the ground follows them.
    @objc private func handleMove(_ g: UIPanGestureRecognizer) {
        switch g.state {
        case .began:
            stopMotion()
        case .changed:
            let t = g.translation(in: self)
            g.setTranslation(.zero, in: self)
            let delta = groundDelta(forScreen: SIMD2(Double(t.x), Double(t.y)))
            orbit.target.x += delta.x
            orbit.target.z += delta.y
            settleTarget()
            applyOrbit()
        case .ended:
            let v = g.velocity(in: self)
            moveVelocity = groundDelta(forScreen: SIMD2(Double(v.x), Double(v.y)))
            startLink()
        default:
            break
        }
    }

    /// The target's move for a screen drag: dragging right moves the ground right (the target
    /// left), dragging down pulls the ground nearer (the target away).
    private func groundDelta(forScreen d: SIMD2<Double>) -> SIMD2<Double> {
        let s = moveScale
        return orbit.groundRight * (-d.x * s.x) + orbit.groundForward * (d.y * s.y)
    }

    @objc private func handlePinch(_ g: UIPinchGestureRecognizer) {
        switch g.state {
        case .began:
            stopMotion()
            pinchStartDistance = orbit.distance
        case .changed:
            orbit.distance = pinchStartDistance / max(Double(g.scale), 0.01)
            applyOrbit()
        default:
            break
        }
    }

    /// Twist: the scene turns with the fingers.
    @objc private func handleTwist(_ g: UIRotationGestureRecognizer) {
        switch g.state {
        case .began:
            stopMotion()
            twistStartYaw = orbit.yaw
        case .changed:
            orbit.yaw = twistStartYaw - Double(g.rotation)
            applyOrbit()
        default:
            break
        }
    }

    /// Double tap: twice as close, centred where the tap meets the terrain.
    @objc private func handleDoubleTap(_ g: UITapGestureRecognizer) {
        var to = orbit
        to.distance = orbit.distance / 2
        if let hit = terrainPoint(at: g.location(in: scnView)) { to.target = hit }
        animate(to: to, duration: 0.35)
    }

    @objc private func handleTwoFingerTap(_ g: UITapGestureRecognizer) {
        var to = orbit
        to.distance = orbit.distance * 2
        animate(to: to, duration: 0.35)
    }

    /// Where a view point meets the terrain (scene coordinates).
    private func terrainPoint(at point: CGPoint) -> SIMD3<Double>? {
        let options: [SCNHitTestOption: Any] = [
            .categoryBitMask: Self.terrainCategory,
            .searchMode: SCNHitTestSearchMode.closest.rawValue,
        ]
        guard let hit = scnView.hitTest(point, options: options).first else { return nil }
        let w = hit.worldCoordinates
        return SIMD3(Double(w.x), Double(w.y), Double(w.z))
    }
}

/// One billboard: a marker's nodes and its place on the block (height not exaggerated).
@MainActor
private final class Terrain3DSprite {
    let marker: Terrain3DMarker
    let anchor: SCNNode
    let disc: SCNNode
    let label: SCNNode?
    let local: SIMD3<Double>
    /// The disc's centre above the anchor, points.
    let discLift: CGFloat
    let labelSize: CGSize?
    /// Placed last time (decluttering keeps these first among equals).
    var shown = false

    init(marker: Terrain3DMarker, anchor: SCNNode, disc: SCNNode, label: SCNNode?, local: SIMD3<Double>,
         discLift: CGFloat, labelSize: CGSize?) {
        self.marker = marker
        self.anchor = anchor
        self.disc = disc
        self.label = label
        self.local = local
        self.discLift = discLift
        self.labelSize = labelSize
    }
}

/// Breaks the CADisplayLink → target retain cycle.
@MainActor
private final class Terrain3DLinkProxy: NSObject {
    weak var view: Terrain3DSceneUIView?

    init(_ view: Terrain3DSceneUIView) {
        self.view = view
    }

    @objc func tick(_ link: CADisplayLink) {
        if let view {
            view.step(link)
        } else {
            link.invalidate()
        }
    }
}

// MARK: - Art

/// Billboard pictures: the map's POI markers (`OverlayArt`), and the same style for waypoints
/// (red, their symbol) and notes (amber). Main thread only (cached).
@MainActor
enum Terrain3DArt {
    private static var cache: [String: UIImage] = [:]

    static func marker(for style: Terrain3DMarker.Style) -> UIImage {
        switch style {
        case .poi(let kind):
            return OverlayArt.marker(for: kind)
        case .waypoint(let symbol):
            return disc(glyph: symbol.systemImage, color: POIKindStyle.uiColor(for: .waypoint))
        case .note(let photo):
            return disc(glyph: photo ? "camera.fill" : "note.text", color: Theme.uiNote)
        }
    }

    /// A disc in `color` with a white ring, a soft shadow and a white glyph, the size of the map's
    /// POI markers (centred, `OverlayArt.markerPadding` around it).
    static func disc(glyph: String, color: UIColor) -> UIImage {
        let key = "\(glyph)|\(color.description)"
        if let hit = cache[key] { return hit }
        let d = OverlayArt.markerSize
        let pad = OverlayArt.markerPadding
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        let image = UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let cg = ctx.cgContext
            let ring = CGRect(x: pad, y: pad, width: d, height: d)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.35).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: ring).fill()
            cg.restoreGState()
            color.setFill()
            UIBezierPath(ovalIn: ring.insetBy(dx: 2, dy: 2)).fill()
            let config = UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            if let symbol = UIImage(systemName: glyph, withConfiguration: config)?
                .withTintColor(.white, renderingMode: .alwaysOriginal) {
                let s = symbol.size
                symbol.draw(in: CGRect(x: ring.midX - s.width / 2, y: ring.midY - s.height / 2, width: s.width, height: s.height))
            }
        }
        cache[key] = image
        return image
    }
}
