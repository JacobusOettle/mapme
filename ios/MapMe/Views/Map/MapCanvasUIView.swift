import CoreLocation
import QuartzCore
import UIKit

/// The interactive map: a CATiledLayer raster moved by a camera (centre/zoom/rotation applied as the
/// tiled view's centre + transform, never relayout) on a shadowed paper sheet, gestures with
/// inertia, Apple Maps style tracking (none / follow / follow with heading), two-finger rotation and
/// screen-space overlays. A display link runs only while something moves.
final class MapCanvasUIView: UIView, UIGestureRecognizerDelegate {
    struct Inputs {
        var map: MapDocument
        var tileStore: TileStore
        var nightMode: Bool
        var tracking: CanvasTrackingMode
        /// Keep the map paper-up (calibration); otherwise the rotation stays where the user leaves it.
        var lockNorthUp: Bool
        /// Two-finger rotation.
        var allowRotation: Bool
        var userLocation: CLLocation?
        var heading: Double?
        var headingAccuracy: Double?
        var track: [TrackPoint]
        var showTrack: Bool
        var pois: [MapPOI]
        var showPOIs: Bool
        /// Also show POIs read off the map (they are printed on it already).
        var showMapLabels: Bool
        var selectedPOIID: UUID?
        var markers: [CanvasMarker]
        var units: DistanceUnits
        var chromeInsets: UIEdgeInsets
        var locationColor: UIColor
    }

    var onTap: ((CGPoint) -> Void)?
    var onLongPress: ((CGPoint) -> Void)?
    var onSelectPOI: ((MapPOI) -> Void)?
    /// The canvas changed the tracking mode itself (a pan stops following, rotating by hand leaves
    /// heading mode, tapping the compass returns to north up).
    var onTrackingChange: ((CanvasTrackingMode) -> Void)?
    /// Receives rotation readouts (for the compass) and sends commands (north up).
    weak var controller: MapCanvasController? {
        didSet {
            if controller !== oldValue { controller?.canvas = self }
        }
    }

    private let sheetLayer = CALayer()
    private let tiledView = TiledMapView(frame: .zero)
    private let overlay = MapOverlayView(frame: .zero)

    private var map: MapDocument?
    private weak var tileStore: TileStore?
    private var nightMode = false
    private(set) var camera = CanvasCamera(center: .zero, zoom: 1, rotation: 0)
    private var hasInitialCamera = false

    private var tracking: CanvasTrackingMode = .none
    /// Tracking value the canvas reported and is waiting to see echoed back by SwiftUI.
    private var pendingTracking: CanvasTrackingMode?
    private var lockNorthUp = false
    /// Animating back to paper-up after the compass was tapped (or heading mode was left).
    private var resettingNorth = false
    private var followEngaged = false
    private var location: CLLocation?
    private var rawHeading: Double?
    private var headingAccuracy: Double?
    private var selectedPOIID: UUID?
    private var chromeInsets: UIEdgeInsets = .zero
    /// North direction at the middle of the map (radians, clockwise from image-up), for the compass
    /// when you are not on the map.
    private var mapNorthAngle = 0.0
    private var sheetShadowZoom: CGFloat = 0

    // Motion state.
    private var displayLink: CADisplayLink?
    private var lastTimestamp: CFTimeInterval = 0
    private var velocity: CGPoint = .zero
    private var targetCenter: CGPoint?
    private var targetZoom: CGFloat?
    private var zoomAnchor: (map: CGPoint, view: CGPoint)?
    /// Smoothed heading as a unit vector (sin, cos) so 359° → 0° does not spin the long way round.
    private var smoothedHeading: CGVector?
    private var activeGestures = 0
    private var pinchFocal: CGPoint?
    private var rotationFocal: CGPoint?

    private let pan = UIPanGestureRecognizer()
    private let pinch = UIPinchGestureRecognizer()
    private let rotate = UIRotationGestureRecognizer()
    private let doubleTap = UITapGestureRecognizer()
    private let twoFingerTap = UITapGestureRecognizer()
    private let singleTap = UITapGestureRecognizer()
    private let longPress = UILongPressGestureRecognizer()

    static let maxZoom: CGFloat = 6
    /// Comfortable hiking zoom when following: about this many ground metres per point.
    static let followMetersPerPoint = 2.5
    private static let headingTau = 0.12
    private static let rotationTau = 0.09
    private static let moveTau = 0.16
    /// UIScrollView-like deceleration (≈0.998 per ms).
    private static let decelerationRate: CGFloat = 2.0

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = Theme.uiMapBackground
        clipsToBounds = true
        isMultipleTouchEnabled = true
        accessibilityIdentifier = "mapCanvas"
        sheetLayer.backgroundColor = Theme.uiPaper.cgColor
        sheetLayer.shadowColor = UIColor.black.cgColor
        sheetLayer.shadowOpacity = 0.55
        sheetLayer.isHidden = true
        layer.addSublayer(sheetLayer)
        addSubview(tiledView)
        overlay.frame = bounds
        overlay.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(overlay)
        setUpGestures()
        NotificationCenter.default.addObserver(self, selector: #selector(didReceiveMemoryWarning),
                                               name: UIApplication.didReceiveMemoryWarningNotification, object: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    deinit {
        displayLink?.invalidate()
    }

    @objc private func didReceiveMemoryWarning() {
        tileStore?.purgeMemory()
    }

    override func willMove(toWindow newWindow: UIWindow?) {
        super.willMove(toWindow: newWindow)
        if newWindow == nil { stopAnimating() }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.width > 0, bounds.height > 0, let map else { return }
        if !hasInitialCamera {
            hasInitialCamera = true
            setInitialCamera(map: map)
        }
        camera.zoom = clampZoom(camera.zoom)
        camera.center = clampCenter(camera.center)
        applyCamera()
        startAnimating()
    }

    // MARK: - Inputs

    func apply(_ inputs: Inputs) {
        let mapChanged = map?.id != inputs.map.id
        if mapChanged || tileStore !== inputs.tileStore || nightMode != inputs.nightMode {
            tiledView.configure(map: inputs.map, store: inputs.tileStore, dark: inputs.nightMode)
            tileStore = inputs.tileStore
            nightMode = inputs.nightMode
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            let size = inputs.map.pixelSize
            sheetLayer.bounds = CGRect(origin: .zero, size: size)
            sheetLayer.shadowPath = UIBezierPath(rect: sheetLayer.bounds).cgPath
            sheetLayer.backgroundColor = (inputs.nightMode ? Theme.uiNightPaper : Theme.uiPaper).cgColor
            sheetLayer.isHidden = size.width <= 0 || size.height <= 0
            CATransaction.commit()
        }
        if map?.georef != inputs.map.georef || mapChanged {
            let mid = CGPoint(x: inputs.map.pixelSize.width / 2, y: inputs.map.pixelSize.height / 2)
            mapNorthAngle = inputs.map.georef?.northAngle(atPixel: mid) ?? 0
        }
        map = inputs.map
        if mapChanged {
            hasInitialCamera = false
            setNeedsLayout()
        }

        overlay.units = inputs.units
        overlay.setStyle(locationColor: inputs.locationColor, night: inputs.nightMode)
        overlay.setGeoref(inputs.map.georef)
        let hadTarget = followTarget != nil
        location = inputs.userLocation
        overlay.setLocation(inputs.userLocation)
        overlay.setTrack(inputs.track, visible: inputs.showTrack)
        overlay.setPOIs(inputs.pois, visible: inputs.showPOIs, showMapLabels: inputs.showMapLabels)
        overlay.setSelected(inputs.selectedPOIID)
        overlay.setMarkers(inputs.markers)
        rawHeading = inputs.heading
        headingAccuracy = inputs.headingAccuracy

        if inputs.chromeInsets != chromeInsets {
            chromeInsets = inputs.chromeInsets
            setNeedsLayout()
        }

        lockNorthUp = inputs.lockNorthUp
        let canRotate = inputs.allowRotation && !inputs.lockNorthUp
        if rotate.isEnabled != canRotate { rotate.isEnabled = canRotate }

        // Tracking (ignore stale values until SwiftUI echoes the one we reported).
        if let pending = pendingTracking {
            if inputs.tracking == pending { pendingTracking = nil }
        } else if inputs.tracking != tracking {
            // Like Apple Maps: leaving heading mode with the button turns the map back to north.
            let leavingHeading = tracking == .followWithHeading && inputs.tracking == .follow
            setTracking(inputs.tracking, notify: false)
            if leavingHeading { resettingNorth = true }
        }
        if tracking.follows, !hadTarget, followTarget != nil {
            engageFollow()
        }

        if inputs.selectedPOIID != selectedPOIID {
            selectedPOIID = inputs.selectedPOIID
            if let id = inputs.selectedPOIID { framePOI(id) }
        }
        startAnimating()
    }

    private func setTracking(_ value: CanvasTrackingMode, notify: Bool) {
        guard value != tracking else { return }
        let wasFollowing = tracking.follows
        tracking = value
        if value.follows, !wasFollowing {
            velocity = .zero
            targetCenter = nil
            zoomAnchor = nil
            followEngaged = false
            engageFollow()
        } else if !value.follows {
            followEngaged = false
        }
        if value == .followWithHeading { resettingNorth = false }
        if notify {
            pendingTracking = value
            onTrackingChange?(value)
        }
        startAnimating()
    }

    /// Start following: glide to the user and zoom in to a walking scale if zoomed far out.
    private func engageFollow() {
        guard followTarget != nil, !followEngaged else { return }
        followEngaged = true
        let comfortable = comfortableZoom()
        if camera.zoom < comfortable * 0.5 { targetZoom = comfortable }
        startAnimating()
    }

    /// Compass tap: turn the map back to paper-up (north-up for a grid-aligned map).
    func animateToNorthUp() {
        if tracking == .followWithHeading { setTracking(.follow, notify: true) }
        resettingNorth = true
        startAnimating()
    }

    // MARK: - Geometry

    private var mapSize: CGSize { map?.pixelSize ?? .zero }

    /// The user's map pixel when it is on (or just beside) the map; following is a no-op otherwise
    /// (the off-screen arrow and the stats panel say where the map is instead).
    private var followTarget: CGPoint? {
        guard let u = overlay.userMapPixel else { return nil }
        let margin = max(mapSize.width, mapSize.height) * 0.1
        return CGRect(origin: .zero, size: mapSize).insetBy(dx: -margin, dy: -margin).contains(u) ? u : nil
    }

    /// The part of the view not covered by floating controls.
    private var contentRect: CGRect {
        let s = safeAreaInsets
        let c = chromeInsets
        let insets = UIEdgeInsets(top: max(s.top, c.top), left: max(s.left, c.left),
                                  bottom: max(s.bottom, c.bottom), right: max(s.right, c.right))
        let r = bounds.inset(by: insets)
        return r.width > 40 && r.height > 40 ? r : bounds
    }

    private var fitZoom: CGFloat {
        let r = contentRect
        guard mapSize.width > 0, mapSize.height > 0 else { return 1 }
        return min(r.width / mapSize.width, r.height / mapSize.height) * 0.95
    }

    private var minZoom: CGFloat { min(fitZoom * 0.9, Self.maxZoom) }

    private func clampZoom(_ z: CGFloat) -> CGFloat {
        guard z.isFinite else { return fitZoom }
        return min(max(z, minZoom), Self.maxZoom)
    }

    /// Keeps the middle of the screen over the map, so the map can never leave the screen entirely.
    private func clampCenter(_ c: CGPoint) -> CGPoint {
        guard c.x.isFinite, c.y.isFinite else { return CGPoint(x: mapSize.width / 2, y: mapSize.height / 2) }
        return CGPoint(x: min(max(c.x, 0), mapSize.width), y: min(max(c.y, 0), mapSize.height))
    }

    /// Where the puck sits while following: centred, or lower in heading mode so you see ahead.
    private var followAnchor: CGPoint {
        let r = contentRect
        return CGPoint(x: bounds.midX, y: r.minY + r.height * (tracking == .followWithHeading ? 0.62 : 0.5))
    }

    private func comfortableZoom() -> CGFloat {
        let mpp = overlay.userMetersPerPixel
        return clampZoom(CGFloat(mpp / Self.followMetersPerPoint))
    }

    /// Map-pixel offset for a view-space drag of `d` points.
    private func mapDelta(forViewDelta d: CGPoint) -> CGPoint {
        let c = cos(camera.rotation), s = sin(camera.rotation)
        return CGPoint(x: (d.x * c + d.y * s) / camera.zoom, y: (-d.x * s + d.y * c) / camera.zoom)
    }

    private func setInitialCamera(map: MapDocument) {
        velocity = .zero
        targetCenter = nil
        targetZoom = nil
        zoomAnchor = nil
        let size = bounds.size
        if tracking == .followWithHeading, let heading = smoothedHeadingDegrees ?? effectiveHeading {
            camera.rotation = CGFloat(MapAngle.normalized(-(MapAngle.radians(heading) + overlay.userNorthAngle)))
        } else {
            camera.rotation = 0
        }
        if tracking.follows, let user = followTarget, CGRect(origin: .zero, size: mapSize).contains(user) {
            camera.zoom = comfortableZoom()
            camera.center = clampCenter(camera.center(placing: user, at: followAnchor, viewSize: size))
            followEngaged = true
        } else {
            camera.zoom = fitZoom
            let mid = CGPoint(x: mapSize.width / 2, y: mapSize.height / 2)
            camera.center = camera.center(placing: mid, at: CGPoint(x: contentRect.midX, y: contentRect.midY), viewSize: size)
        }
    }

    /// Frame the selected POI together with the user (if the user is near the map).
    private func framePOI(_ id: UUID) {
        guard let p = overlay.pixel(ofPOI: id), bounds.width > 0 else { return }
        if tracking.follows { setTracking(.none, notify: true) }
        velocity = .zero
        zoomAnchor = nil
        let r = contentRect
        let target: CGPoint
        if let u = overlay.userMapPixel,
           CGRect(origin: .zero, size: mapSize).insetBy(dx: -mapSize.width * 0.25, dy: -mapSize.height * 0.25).contains(u) {
            let d = max(1, hypot(u.x - p.x, u.y - p.y))
            let z = clampZoom(min(min(r.width, r.height) * 0.7 / d, comfortableZoom() * 2))
            targetZoom = z
            target = CGPoint(x: (u.x + p.x) / 2, y: (u.y + p.y) / 2)
        } else {
            targetZoom = max(camera.zoom, comfortableZoom())
            target = p
        }
        var future = camera
        future.zoom = targetZoom ?? camera.zoom
        targetCenter = clampCenter(future.center(placing: target, at: CGPoint(x: r.midX, y: r.midY), viewSize: bounds.size))
        startAnimating()
    }

    // MARK: - Rendering

    private var smoothedHeadingDegrees: Double? {
        guard let v = smoothedHeading else { return nil }
        return atan2(Double(v.dx), Double(v.dy)) * 180 / .pi
    }

    /// Compass heading, or the GPS course when moving without a compass.
    private var effectiveHeading: Double? {
        if let rawHeading { return rawHeading }
        if let l = location, l.course >= 0, l.speed > 0.7 { return l.course }
        return nil
    }

    private func applyCamera() {
        guard map != nil else { return }
        let size = bounds.size
        let mid = CGPoint(x: mapSize.width / 2, y: mapSize.height / 2)
        let center = camera.viewPoint(forMap: mid, viewSize: size)
        let transform = CGAffineTransform(rotationAngle: camera.rotation).scaledBy(x: camera.zoom, y: camera.zoom)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        tiledView.center = center
        tiledView.transform = transform
        sheetLayer.position = center
        sheetLayer.setAffineTransform(transform)
        // The sheet's shadow lives in map pixels; keep it about 16 pt on screen.
        if sheetShadowZoom <= 0 || abs(camera.zoom / sheetShadowZoom - 1) > 0.05 {
            sheetShadowZoom = camera.zoom
            let z = max(camera.zoom, 1e-4)
            sheetLayer.shadowRadius = 16 / z
            sheetLayer.shadowOffset = CGSize(width: 0, height: 5 / z)
        }
        CATransaction.commit()
        overlay.render(camera: camera, contentRect: contentRect, heading: smoothedHeadingDegrees, headingAccuracy: headingAccuracy)
        if let controller {
            let north = (overlay.userMapPixel != nil ? overlay.userNorthAngle : mapNorthAngle) + Double(camera.rotation)
            controller.publish(rotationDegrees: Double(camera.rotation) * 180 / .pi, northDegrees: north * 180 / .pi)
        }
    }

    private func startAnimating() {
        guard displayLink == nil, window != nil else {
            if window == nil { applyCamera() }
            return
        }
        let link = CADisplayLink(target: DisplayLinkProxy(self), selector: #selector(DisplayLinkProxy.tick(_:)))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        displayLink = link
        lastTimestamp = 0
    }

    private func stopAnimating() {
        displayLink?.invalidate()
        displayLink = nil
    }

    /// One animation frame. Returns to idle (link stopped) once nothing changes any more.
    fileprivate func step(_ link: CADisplayLink) {
        let now = link.timestamp
        let dt = lastTimestamp == 0 ? 1.0 / 60 : min(max(now - lastTimestamp, 1.0 / 240), 1.0 / 15)
        lastTimestamp = now
        var active = false
        guard map != nil, bounds.width > 0 else {
            stopAnimating()
            return
        }
        let size = bounds.size

        // The puck glides between fixes; the camera follows the glide.
        if overlay.advance(now: now, dt: dt) { active = true }

        // Heading low-pass on the unit vector.
        if let target = effectiveHeading {
            let t = MapAngle.radians(target)
            let tv = CGVector(dx: sin(t), dy: cos(t))
            if var v = smoothedHeading {
                let a = CGFloat(1 - exp(-dt / Self.headingTau))
                v.dx += (tv.dx - v.dx) * a
                v.dy += (tv.dy - v.dy) * a
                let n = max(1e-9, hypot(v.dx, v.dy))
                v = CGVector(dx: v.dx / n, dy: v.dy / n)
                let diff = abs(MapAngle.normalized(atan2(Double(v.dx), Double(v.dy)) - t))
                if diff > 0.003 {
                    active = true
                    smoothedHeading = v
                } else {
                    smoothedHeading = tv
                }
            } else {
                smoothedHeading = tv
            }
        } else {
            smoothedHeading = nil
        }

        // Inertia after a pan.
        if activeGestures == 0, velocity != .zero {
            let d = mapDelta(forViewDelta: CGPoint(x: velocity.x * CGFloat(dt), y: velocity.y * CGFloat(dt)))
            let before = camera.center
            camera.center = clampCenter(CGPoint(x: camera.center.x - d.x, y: camera.center.y - d.y))
            let decay = exp(-Self.decelerationRate * CGFloat(dt))
            velocity = CGPoint(x: velocity.x * decay, y: velocity.y * decay)
            if hypot(velocity.x, velocity.y) < 12 || camera.center == before {
                velocity = .zero
            } else {
                active = true
                overlay.flashScaleBar()
            }
        }

        // Rotation (around the puck while following, so it stays put).
        if activeGestures == 0 || tracking == .followWithHeading, let target = targetRotation() {
            let pivot = tracking.follows && activeGestures == 0 ? followTarget : nil
            let pivotView = pivot.map { camera.viewPoint(forMap: $0, viewSize: size) }
            let diff = CGFloat(MapAngle.normalized(Double(target - camera.rotation)))
            if abs(diff) > 0.0015 {
                camera.rotation += diff * CGFloat(1 - exp(-dt / Self.rotationTau))
                active = true
            } else {
                camera.rotation = target
                if resettingNorth, target == 0 { resettingNorth = false }
            }
            if let pivot, let pivotView {
                camera.center = clampCenter(camera.center(placing: pivot, at: pivotView, viewSize: size))
            }
        }

        // Zoom animation (double tap, framing, follow engage).
        if let tz = targetZoom {
            let z = clampZoom(tz)
            let ratio = log(z / camera.zoom)
            if abs(ratio) > 0.002 {
                camera.zoom *= exp(ratio * CGFloat(1 - exp(-dt / Self.moveTau)))
                active = true
            } else {
                camera.zoom = z
                targetZoom = nil
            }
            if let anchor = zoomAnchor {
                camera.center = clampCenter(camera.center(placing: anchor.map, at: anchor.view, viewSize: size))
            }
            if targetZoom == nil { zoomAnchor = nil }
        }

        // Centre: follow the user, or glide to a target.
        if tracking.follows, activeGestures == 0, let user = followTarget {
            let desired = clampCenter(camera.center(placing: user, at: followAnchor, viewSize: size))
            active = glide(to: desired, dt: dt) || active
        } else if let tc = targetCenter, activeGestures == 0 {
            if !glide(to: tc, dt: dt) { targetCenter = nil } else { active = true }
        }

        applyCamera()
        if !active { stopAnimating() }
    }

    /// Eases the centre toward `target`; returns true while still moving.
    private func glide(to target: CGPoint, dt: Double) -> Bool {
        let dx = target.x - camera.center.x, dy = target.y - camera.center.y
        if hypot(dx, dy) * camera.zoom < 0.25 {
            camera.center = target
            return false
        }
        let a = CGFloat(1 - exp(-dt / Self.moveTau))
        camera.center = CGPoint(x: camera.center.x + dx * a, y: camera.center.y + dy * a)
        return true
    }

    private func targetRotation() -> CGFloat? {
        if tracking == .followWithHeading, let h = smoothedHeadingDegrees {
            return CGFloat(MapAngle.normalized(-(MapAngle.radians(h) + overlay.userNorthAngle)))
        }
        if lockNorthUp || resettingNorth { return 0 }
        return nil
    }

    // MARK: - Gestures

    private func setUpGestures() {
        pan.addTarget(self, action: #selector(handlePan(_:)))
        pinch.addTarget(self, action: #selector(handlePinch(_:)))
        rotate.addTarget(self, action: #selector(handleRotation(_:)))
        doubleTap.addTarget(self, action: #selector(handleDoubleTap(_:)))
        twoFingerTap.addTarget(self, action: #selector(handleTwoFingerTap(_:)))
        singleTap.addTarget(self, action: #selector(handleTap(_:)))
        longPress.addTarget(self, action: #selector(handleLongPress(_:)))

        doubleTap.numberOfTapsRequired = 2
        twoFingerTap.numberOfTouchesRequired = 2
        singleTap.require(toFail: doubleTap)
        longPress.minimumPressDuration = 0.5
        rotate.isEnabled = false

        for g in [pan, pinch, rotate, doubleTap, twoFingerTap, singleTap, longPress] as [UIGestureRecognizer] {
            g.delegate = self
            addGestureRecognizer(g)
        }
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer) -> Bool {
        let continuous: [UIGestureRecognizer] = [pan, pinch, rotate]
        return continuous.contains(gestureRecognizer) && continuous.contains(otherGestureRecognizer)
    }

    private func gestureBegan() {
        activeGestures += 1
        velocity = .zero
        targetCenter = nil
        targetZoom = nil
        zoomAnchor = nil
    }

    private func gestureEnded() {
        activeGestures = max(0, activeGestures - 1)
        startAnimating()
    }

    @objc private func handlePan(_ g: UIPanGestureRecognizer) {
        switch g.state {
        case .began:
            gestureBegan()
            if tracking.follows { setTracking(.none, notify: true) }
        case .changed:
            let t = g.translation(in: self)
            g.setTranslation(.zero, in: self)
            // While pinching or rotating, their focal point moves the map instead.
            let busy = { (r: UIGestureRecognizer) -> Bool in r.state == .began || r.state == .changed }
            guard !busy(pinch), !busy(rotate) else { return }
            let d = mapDelta(forViewDelta: t)
            camera.center = clampCenter(CGPoint(x: camera.center.x - d.x, y: camera.center.y - d.y))
            overlay.flashScaleBar()
            applyCamera()
        case .ended:
            let v = g.velocity(in: self)
            let speed = hypot(v.x, v.y)
            let maxSpeed: CGFloat = 6000
            velocity = speed > 60 ? CGPoint(x: v.x * min(1, maxSpeed / speed), y: v.y * min(1, maxSpeed / speed)) : .zero
            gestureEnded()
        case .cancelled, .failed:
            gestureEnded()
        default:
            break
        }
    }

    @objc private func handlePinch(_ g: UIPinchGestureRecognizer) {
        switch g.state {
        case .began:
            gestureBegan()
            pinchFocal = g.location(in: self)
        case .changed:
            let focal = g.location(in: self)
            let size = bounds.size
            let newZoom = clampZoom(camera.zoom * g.scale)
            g.scale = 1
            if tracking.follows, let user = followTarget {
                // Zoom around your position so following continues.
                camera.zoom = newZoom
                camera.center = clampCenter(camera.center(placing: user, at: followAnchor, viewSize: size))
            } else {
                let previous = g.numberOfTouches >= 2 ? (pinchFocal ?? focal) : focal
                let anchor = camera.mapPoint(forView: previous, viewSize: size)
                camera.zoom = newZoom
                camera.center = clampCenter(camera.center(placing: anchor, at: focal, viewSize: size))
            }
            pinchFocal = focal
            overlay.flashScaleBar()
            applyCamera()
        case .ended, .cancelled, .failed:
            pinchFocal = nil
            gestureEnded()
        default:
            break
        }
    }

    @objc private func handleRotation(_ g: UIRotationGestureRecognizer) {
        switch g.state {
        case .began:
            gestureBegan()
            rotationFocal = g.location(in: self)
            resettingNorth = false
            // Turning the map by hand leaves heading mode (still following), like Apple Maps.
            if tracking == .followWithHeading { setTracking(.follow, notify: true) }
        case .changed:
            guard !lockNorthUp else { return }
            let focal = g.location(in: self)
            let size = bounds.size
            let previous = g.numberOfTouches >= 2 ? (rotationFocal ?? focal) : focal
            let user = tracking.follows ? followTarget : nil
            let anchor = user ?? camera.mapPoint(forView: previous, viewSize: size)
            camera.rotation = CGFloat(MapAngle.normalized(Double(camera.rotation + g.rotation)))
            g.rotation = 0
            let at = user != nil ? followAnchor : focal
            camera.center = clampCenter(camera.center(placing: anchor, at: at, viewSize: size))
            rotationFocal = focal
            applyCamera()
        case .ended, .cancelled, .failed:
            rotationFocal = nil
            gestureEnded()
        default:
            break
        }
    }

    @objc private func handleDoubleTap(_ g: UITapGestureRecognizer) {
        animateZoom(by: 2, around: g.location(in: self))
    }

    @objc private func handleTwoFingerTap(_ g: UITapGestureRecognizer) {
        animateZoom(by: 0.5, around: g.location(in: self))
    }

    private func animateZoom(by factor: CGFloat, around point: CGPoint) {
        velocity = .zero
        targetCenter = nil
        let target = clampZoom((targetZoom ?? camera.zoom) * factor)
        targetZoom = target
        zoomAnchor = tracking.follows && followTarget != nil ? nil : (map: camera.mapPoint(forView: point, viewSize: bounds.size), view: point)
        overlay.flashScaleBar()
        startAnimating()
    }

    @objc private func handleTap(_ g: UITapGestureRecognizer) {
        let p = g.location(in: self)
        if let poi = overlay.poi(at: p) {
            onSelectPOI?(poi)
        } else {
            onTap?(camera.mapPoint(forView: p, viewSize: bounds.size))
        }
    }

    @objc private func handleLongPress(_ g: UILongPressGestureRecognizer) {
        guard g.state == .began else { return }
        onLongPress?(camera.mapPoint(forView: g.location(in: self), viewSize: bounds.size))
    }
}

/// Breaks the CADisplayLink → target retain cycle.
@MainActor
private final class DisplayLinkProxy: NSObject {
    weak var canvas: MapCanvasUIView?

    init(_ canvas: MapCanvasUIView) {
        self.canvas = canvas
    }

    @objc func tick(_ link: CADisplayLink) {
        if let canvas {
            canvas.step(link)
        } else {
            link.invalidate()
        }
    }
}
