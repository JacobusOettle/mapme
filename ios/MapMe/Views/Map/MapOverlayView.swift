import CoreLocation
import QuartzCore
import UIKit

/// Screen-space overlays drawn above the (transformed) map: GPS dot, accuracy circle, heading cone,
/// dotted trail, POIs, selection guide line, calibration pins, scale bar and the off-screen arrow.
/// Inputs are set when they change; `render` positions everything for a camera and is cheap
/// enough to run every frame (no big allocations, implicit animations disabled).
final class MapOverlayView: UIView {
    var units: DistanceUnits = .metric {
        didSet {
            guard units != oldValue else { return }
            scaleSpec = nil
            rebuildPOIs()
        }
    }

    // MARK: Inputs

    private var georef: Georeference?
    private var mercatorToPixel: Homography = .identity
    private var location: CLLocation?
    private var userPixel: CGPoint?
    private var userLatLon: LatLon?
    private var metersPerPixelAtUser = 1.0
    private var northAtUser = 0.0

    private var trackPoints: [TrackPoint] = []
    private var trackPixels: [CGPoint] = []
    private var showTrack = true
    private var trailDirty = true

    private var pois: [MapPOI] = []
    private var items: [POIItem] = []
    private var showPOIs = true
    private var labelsDirty = true
    private var selectedID: UUID?

    private var markers: [CanvasMarker] = []
    private var markerLayers: [CALayer] = []

    // MARK: Layers

    private let trailCasing = CAShapeLayer()
    private let trailLayer = CAShapeLayer()
    private let accuracyLayer = CALayer()
    private let coneLayer = CAGradientLayer()
    private let coneMask = CAShapeLayer()
    private let poiContainer = CALayer()
    private let selectionRing = CALayer()
    private let guideLayer = CAShapeLayer()
    private let guideLabel = CALayer()
    private let haloLayer = CALayer()
    private let dotLayer = CALayer()
    private let markerContainer = CALayer()
    private let offscreenArrow = CAShapeLayer()
    private let offscreenLabel = CALayer()
    private let scaleBarLayer = CAShapeLayer()
    private let scaleLabel = CALayer()

    // MARK: Render caches

    private var lastCamera: MapCamera?
    private var lastLabelCamera: MapCamera?
    private var coneWidth: CGFloat = -1
    private var guideText = ""
    private var renderedGuideText = ""
    private var offscreenText = ""
    private var scaleSpec: ScaleBarSpec?
    private var scaleZoom: CGFloat = 0
    private var scaleOrigin: CGPoint = .zero

    static let dotSize: CGFloat = 24
    static let coneRadius: CGFloat = 64
    /// Labels are only drawn when zoomed in at least this far (points per map pixel)…
    static let labelMinZoom: CGFloat = 0.16
    /// …and the trail skips points closer than this on screen.
    static let trailMinSpacing: CGFloat = 2.5

    private final class POIItem {
        let poi: MapPOI
        let pixel: CGPoint
        let priority: Int
        let badge = CALayer()
        let label = CALayer()
        var screen: CGPoint = .zero
        var labelVisible = false

        init(poi: MapPOI, pixel: CGPoint) {
            self.poi = poi
            self.pixel = pixel
            switch poi.kind {
            case .waypoint: priority = 0
            case .peak: priority = 1
            case .place, .viewpoint, .shelter: priority = 2
            case .water, .parking: priority = 3
            case .spotHeight, .other: priority = 4
            }
        }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        backgroundColor = .clear
        setUpLayers()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { startPulse() }
    }

    private func setUpLayers() {
        let red = Theme.uiUserRed

        // Round caps on near-zero dashes draw dots; the darker casing underneath keeps them readable on any map.
        for (l, color, width) in [(trailCasing, UIColor.black.withAlphaComponent(0.45), CGFloat(7.5)),
                                  (trailLayer, Theme.uiTrack, CGFloat(5))] {
            l.fillColor = nil
            l.strokeColor = color.cgColor
            l.lineWidth = width
            l.lineCap = .round
            l.lineJoin = .round
            l.lineDashPattern = [0.01, 9]
        }

        accuracyLayer.backgroundColor = red.withAlphaComponent(0.13).cgColor
        accuracyLayer.borderColor = red.withAlphaComponent(0.45).cgColor
        accuracyLayer.borderWidth = 1

        let d = Self.coneRadius * 2
        coneLayer.bounds = CGRect(x: 0, y: 0, width: d, height: d)
        coneLayer.type = .radial
        coneLayer.startPoint = CGPoint(x: 0.5, y: 0.5)
        coneLayer.endPoint = CGPoint(x: 1, y: 1)
        coneLayer.colors = [red.withAlphaComponent(0.6).cgColor, red.withAlphaComponent(0.0).cgColor]
        coneMask.frame = coneLayer.bounds
        coneMask.fillColor = UIColor.black.cgColor
        coneLayer.mask = coneMask

        selectionRing.bounds = CGRect(x: 0, y: 0, width: 38, height: 38)
        selectionRing.cornerRadius = 19
        selectionRing.borderWidth = 2.5
        selectionRing.borderColor = Theme.uiAccent.cgColor
        selectionRing.backgroundColor = Theme.uiAccent.withAlphaComponent(0.18).cgColor
        selectionRing.shadowColor = Theme.uiAccent.cgColor
        selectionRing.shadowOpacity = 0.8
        selectionRing.shadowRadius = 6
        selectionRing.shadowOffset = .zero
        selectionRing.shadowPath = UIBezierPath(ovalIn: selectionRing.bounds).cgPath

        guideLayer.fillColor = nil
        guideLayer.strokeColor = Theme.uiAccent.withAlphaComponent(0.9).cgColor
        guideLayer.lineWidth = 2.5
        guideLayer.lineCap = .round
        guideLayer.lineDashPattern = [7, 6]

        let s = Self.dotSize
        haloLayer.bounds = CGRect(x: 0, y: 0, width: s, height: s)
        haloLayer.cornerRadius = s / 2
        haloLayer.backgroundColor = red.cgColor
        haloLayer.opacity = 0

        dotLayer.bounds = CGRect(x: 0, y: 0, width: s, height: s)
        dotLayer.cornerRadius = s / 2
        dotLayer.backgroundColor = red.cgColor
        dotLayer.borderColor = UIColor.white.cgColor
        dotLayer.borderWidth = 3
        dotLayer.shadowColor = red.cgColor
        dotLayer.shadowOpacity = 0.9
        dotLayer.shadowRadius = 8
        dotLayer.shadowOffset = .zero
        dotLayer.shadowPath = UIBezierPath(ovalIn: dotLayer.bounds).cgPath

        offscreenArrow.path = OverlayArt.arrowPath()
        offscreenArrow.fillColor = red.cgColor
        offscreenArrow.strokeColor = UIColor.white.cgColor
        offscreenArrow.lineWidth = 2
        offscreenArrow.lineJoin = .round
        offscreenArrow.shadowColor = UIColor.black.cgColor
        offscreenArrow.shadowOpacity = 0.5
        offscreenArrow.shadowRadius = 4
        offscreenArrow.shadowOffset = .zero
        offscreenArrow.shadowPath = offscreenArrow.path

        scaleBarLayer.fillColor = nil
        scaleBarLayer.strokeColor = UIColor.white.cgColor
        scaleBarLayer.lineWidth = 2.5
        scaleBarLayer.lineCap = .round
        scaleBarLayer.lineJoin = .round
        scaleLabel.anchorPoint = CGPoint(x: 0, y: 1)

        let stack: [CALayer] = [trailCasing, trailLayer, guideLayer, accuracyLayer, coneLayer, poiContainer, selectionRing,
                                haloLayer, dotLayer, guideLabel, markerContainer, offscreenArrow, offscreenLabel,
                                scaleBarLayer, scaleLabel]
        for l in stack {
            layer.addSublayer(l)
        }
        let initiallyHidden: [CALayer] = [trailCasing, trailLayer, accuracyLayer, coneLayer, selectionRing, haloLayer, dotLayer,
                                          guideLayer, guideLabel, offscreenArrow, offscreenLabel, scaleBarLayer, scaleLabel]
        for l in initiallyHidden {
            l.isHidden = true
        }
    }

    private func startPulse() {
        guard haloLayer.animation(forKey: "pulse") == nil else { return }
        let scale = CABasicAnimation(keyPath: "transform.scale")
        scale.fromValue = 1.0
        scale.toValue = 3.2
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 0.5
        fade.toValue = 0.0
        let group = CAAnimationGroup()
        group.animations = [scale, fade]
        group.duration = 2.2
        group.repeatCount = .infinity
        group.timingFunction = CAMediaTimingFunction(name: .easeOut)
        group.isRemovedOnCompletion = false
        haloLayer.add(group, forKey: "pulse")
    }

    // MARK: - Inputs

    func setGeoref(_ g: Georeference?) {
        guard g != georef else { return }
        georef = g
        mercatorToPixel = g?.mercatorToPixel ?? .identity
        recomputeUser()
        trackPixels = []
        convertTrack(from: 0)
        rebuildPOIs()
        scaleSpec = nil
    }

    func setLocation(_ fix: CLLocation?) {
        if let a = location, let b = fix, a.timestamp == b.timestamp, a.coordinate.latitude == b.coordinate.latitude,
           a.coordinate.longitude == b.coordinate.longitude, a.horizontalAccuracy == b.horizontalAccuracy { return }
        if location == nil, fix == nil { return }
        location = fix
        recomputeUser()
    }

    func setTrack(_ points: [TrackPoint], visible: Bool) {
        if visible != showTrack {
            showTrack = visible
            trailDirty = true
        }
        if points.count == trackPoints.count, points.first == trackPoints.first, points.last == trackPoints.last { return }
        let appended = points.count > trackPoints.count && !trackPoints.isEmpty
            && points.first == trackPoints.first && points[trackPoints.count - 1] == trackPoints.last
        let start = appended ? trackPoints.count : 0
        trackPoints = points
        if !appended { trackPixels = [] }
        convertTrack(from: start)
    }

    func setPOIs(_ list: [MapPOI], visible: Bool) {
        if visible != showPOIs {
            showPOIs = visible
            poiContainer.isHidden = !visible
            labelsDirty = true
        }
        guard list != pois else { return }
        pois = list
        rebuildPOIs()
    }

    func setSelected(_ id: UUID?) {
        guard id != selectedID else { return }
        selectedID = id
        labelsDirty = true
        updateGuideText()
    }

    func setMarkers(_ list: [CanvasMarker]) {
        guard list != markers else { return }
        markers = list
        markerLayers.forEach { $0.removeFromSuperlayer() }
        markerLayers = list.map { m -> CALayer in
            let l = CALayer()
            let image = OverlayArt.pin(label: m.label, color: m.color)
            l.contents = image.cgImage
            l.contentsScale = image.scale
            l.bounds = CGRect(origin: .zero, size: image.size)
            l.anchorPoint = CGPoint(x: 0.5, y: (image.size.height - 1) / image.size.height)
            markerContainer.addSublayer(l)
            return l
        }
    }

    /// The POI drawn nearest to a view point (within finger reach), if any.
    func poi(at point: CGPoint) -> MapPOI? {
        guard showPOIs else { return nil }
        var best: (POIItem, CGFloat)?
        for item in items where !item.badge.isHidden {
            let dx = item.screen.x - point.x, dy = item.screen.y - point.y
            let d2 = dx * dx + dy * dy
            if d2 < 24 * 24, d2 < (best?.1 ?? .infinity) { best = (item, d2) }
        }
        return best?.0.poi
    }

    /// Map pixel of a POI as currently placed (nil if not on this map's georeference).
    func pixel(ofPOI id: UUID) -> CGPoint? { items.first { $0.poi.id == id }?.pixel }

    var userMapPixel: CGPoint? { userPixel }
    var userMetersPerPixel: Double { metersPerPixelAtUser }
    var userNorthAngle: Double { northAtUser }

    // MARK: - Derived data

    private func recomputeUser() {
        guard let georef, let fix = location else {
            userPixel = nil
            userLatLon = nil
            updateGuideText()
            return
        }
        let ll = LatLon(fix.coordinate)
        let px = mercatorToPixel.apply(WebMercator.project(ll))
        guard px.x.isFinite, px.y.isFinite else {
            userPixel = nil
            userLatLon = nil
            return
        }
        let p = CGPoint(x: px.x, y: px.y)
        userPixel = p
        userLatLon = ll
        let mpp = georef.metersPerPixel(atPixel: p)
        metersPerPixelAtUser = mpp.isFinite && mpp > 0 ? mpp : 1
        northAtUser = georef.northAngle(atPixel: p)
        updateGuideText()
    }

    private func convertTrack(from start: Int) {
        trailDirty = true
        guard georef != nil, start < trackPoints.count else { return }
        trackPixels.reserveCapacity(trackPoints.count)
        for i in start..<trackPoints.count {
            let m = mercatorToPixel.apply(WebMercator.project(trackPoints[i].latLon))
            trackPixels.append(CGPoint(x: m.x, y: m.y))
        }
    }

    private func rebuildPOIs() {
        items.forEach {
            $0.badge.removeFromSuperlayer()
            $0.label.removeFromSuperlayer()
        }
        items = []
        labelsDirty = true
        guard georef != nil else { return }
        var built: [POIItem] = []
        built.reserveCapacity(pois.count)
        for poi in pois {
            let m = mercatorToPixel.apply(WebMercator.project(poi.position))
            guard m.x.isFinite, m.y.isFinite else { continue }
            let item = POIItem(poi: poi, pixel: CGPoint(x: m.x, y: m.y))
            let badge = OverlayArt.badge(for: poi.kind)
            item.badge.contents = badge.cgImage
            item.badge.contentsScale = badge.scale
            item.badge.bounds = CGRect(origin: .zero, size: badge.size)
            var text = poi.name
            if let ele = poi.elevationM, ele.isFinite, !poi.name.contains(String(Int(ele.rounded()))) {
                text += " · " + GeoFormat.elevation(ele, units: units)
            }
            let pill = OverlayArt.pill(text, fontSize: 11)
            item.label.contents = pill.cgImage
            item.label.contentsScale = pill.scale
            item.label.bounds = CGRect(origin: .zero, size: pill.size)
            item.label.anchorPoint = CGPoint(x: 0, y: 0.5)
            item.label.isHidden = true
            poiContainer.addSublayer(item.label)
            poiContainer.addSublayer(item.badge)
            built.append(item)
        }
        // Higher priority first, so it wins label collisions.
        items = built.sorted { $0.priority < $1.priority }
        updateGuideText()
    }

    private func updateGuideText() {
        guard let id = selectedID, let here = userLatLon, let poi = pois.first(where: { $0.id == id }) else {
            guideText = ""
            return
        }
        let d = Geodesy.distance(here, poi.position)
        let bearing = Geodesy.bearing(here, poi.position)
        guideText = "\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(bearing))"
    }

    // MARK: - Render

    /// Position every overlay for `camera`. `heading` is the smoothed true heading in degrees.
    func render(camera: MapCamera, contentRect: CGRect, heading: Double?, headingAccuracy: Double?) {
        let size = bounds.size
        guard size.width > 0, size.height > 0 else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let t = camera.transform(viewSize: size)
        let cameraChanged = camera != lastCamera
        let userPoint = userPixel.map { $0.applying(t) }

        renderUser(at: userPoint, camera: camera, heading: heading, headingAccuracy: headingAccuracy)
        if trailDirty || (cameraChanged && showTrack) { renderTrail(t) }
        renderPOIs(t, camera: camera)
        renderSelection(userPoint: userPoint)
        for (marker, l) in zip(markers, markerLayers) {
            l.position = marker.pixel.applying(t)
        }
        renderScaleBar(camera: camera, contentRect: contentRect)
        renderOffscreen(userPoint: userPoint, camera: camera, contentRect: contentRect)

        lastCamera = camera
        CATransaction.commit()
    }

    private func renderUser(at point: CGPoint?, camera: MapCamera, heading: Double?, headingAccuracy: Double?) {
        guard let point, let fix = location else {
            dotLayer.isHidden = true
            haloLayer.isHidden = true
            accuracyLayer.isHidden = true
            coneLayer.isHidden = true
            return
        }
        dotLayer.isHidden = false
        dotLayer.position = point
        haloLayer.isHidden = false
        haloLayer.position = point

        let radius = CGFloat(max(0, fix.horizontalAccuracy) / metersPerPixelAtUser) * camera.zoom
        if radius > Self.dotSize * 0.7, radius < 4000 {
            accuracyLayer.isHidden = false
            accuracyLayer.bounds = CGRect(x: 0, y: 0, width: radius * 2, height: radius * 2)
            accuracyLayer.cornerRadius = radius
            accuracyLayer.position = point
        } else {
            accuracyLayer.isHidden = true
        }

        if let heading {
            let width = CGFloat(MapAngle.radians(min(max((headingAccuracy ?? 22.5) * 2, 25), 70)))
            if abs(width - coneWidth) > 0.01 {
                coneWidth = width
                coneMask.path = OverlayArt.wedgePath(radius: Self.coneRadius, width: width)
            }
            let angle = MapAngle.radians(heading) + northAtUser + Double(camera.rotation)
            coneLayer.isHidden = false
            coneLayer.position = point
            coneLayer.setAffineTransform(CGAffineTransform(rotationAngle: CGFloat(angle)))
        } else {
            coneLayer.isHidden = true
        }
    }

    private func renderTrail(_ t: CGAffineTransform) {
        trailDirty = false
        guard showTrack, trackPixels.count > 1 else {
            trailLayer.isHidden = true
            trailCasing.isHidden = true
            trailLayer.path = nil
            trailCasing.path = nil
            return
        }
        trailLayer.isHidden = false
        trailCasing.isHidden = false
        let path = CGMutablePath()
        let minD2 = Self.trailMinSpacing * Self.trailMinSpacing
        var last = trackPixels[0].applying(t)
        path.move(to: last)
        let lastIndex = trackPixels.count - 1
        for i in 1...lastIndex {
            let v = trackPixels[i].applying(t)
            let dx = v.x - last.x, dy = v.y - last.y
            if dx * dx + dy * dy < minD2, i != lastIndex { continue }
            path.addLine(to: v)
            last = v
        }
        trailLayer.path = path
        trailCasing.path = path
    }

    private func renderPOIs(_ t: CGAffineTransform, camera: MapCamera) {
        guard showPOIs, !items.isEmpty else { return }
        let visibleRect = bounds.insetBy(dx: -60, dy: -30)
        for item in items {
            let p = item.pixel.applying(t)
            item.screen = p
            let onScreen = visibleRect.contains(p)
            item.badge.isHidden = !onScreen
            if onScreen { item.badge.position = p }
        }
        if labelsDirty || labelCameraMoved(camera) {
            placeLabels(camera: camera)
        }
        for item in items {
            let show = item.labelVisible && !item.badge.isHidden
            item.label.isHidden = !show
            if show { item.label.position = CGPoint(x: item.screen.x + 15, y: item.screen.y) }
        }
    }

    private func labelCameraMoved(_ camera: MapCamera) -> Bool {
        guard let last = lastLabelCamera else { return true }
        return abs(camera.zoom / last.zoom - 1) > 0.04 || abs(MapAngle.normalized(Double(camera.rotation - last.rotation))) > 0.04
    }

    /// Greedy label placement: higher-priority labels first, skipping any that would cover
    /// another label or a badge. Translation never changes overlaps, so this only reruns on zoom/rotation.
    private func placeLabels(camera: MapCamera) {
        labelsDirty = false
        lastLabelCamera = camera
        let zoomedIn = camera.zoom >= Self.labelMinZoom
        let half = OverlayArt.badgeSize / 2
        var placed: [CGRect] = []
        placed.reserveCapacity(64)
        let ordered = items.filter { $0.poi.id == selectedID } + items.filter { $0.poi.id != selectedID }
        for item in ordered {
            item.labelVisible = false
            guard zoomedIn || item.poi.id == selectedID else { continue }
            let size = item.label.bounds.size
            let rect = CGRect(x: item.screen.x + 15, y: item.screen.y - size.height / 2, width: size.width, height: size.height)
            if placed.contains(where: { $0.intersects(rect) }) { continue }
            let hitsBadge = items.contains { other in
                other !== item && abs(other.screen.x - rect.midX) < rect.width / 2 + half
                    && abs(other.screen.y - rect.midY) < rect.height / 2 + half
            }
            if hitsBadge, item.poi.id != selectedID { continue }
            placed.append(rect)
            item.labelVisible = true
        }
    }

    private func renderSelection(userPoint: CGPoint?) {
        guard let id = selectedID, showPOIs, let item = items.first(where: { $0.poi.id == id }) else {
            selectionRing.isHidden = true
            guideLayer.isHidden = true
            guideLabel.isHidden = true
            return
        }
        selectionRing.isHidden = false
        selectionRing.position = item.screen
        guard let userPoint, !guideText.isEmpty else {
            guideLayer.isHidden = true
            guideLabel.isHidden = true
            return
        }
        let path = CGMutablePath()
        path.move(to: userPoint)
        path.addLine(to: item.screen)
        guideLayer.path = path
        guideLayer.isHidden = false
        if guideText != renderedGuideText {
            renderedGuideText = guideText
            let image = OverlayArt.pill(guideText, fontSize: 12, weight: .bold, textColor: .black,
                                        background: Theme.uiAccent.withAlphaComponent(0.95))
            guideLabel.contents = image.cgImage
            guideLabel.contentsScale = image.scale
            guideLabel.bounds = CGRect(origin: .zero, size: image.size)
        }
        let mid = CGPoint(x: (userPoint.x + item.screen.x) / 2, y: (userPoint.y + item.screen.y) / 2)
        let hw = guideLabel.bounds.width / 2 + 8
        let hh = guideLabel.bounds.height / 2 + 60
        guideLabel.position = CGPoint(x: min(max(mid.x, hw), max(hw, bounds.width - hw)),
                                      y: min(max(mid.y, hh), max(hh, bounds.height - hh)))
        guideLabel.isHidden = false
    }

    private func renderScaleBar(camera: MapCamera, contentRect: CGRect) {
        guard let georef else {
            scaleBarLayer.isHidden = true
            scaleLabel.isHidden = true
            return
        }
        let origin = CGPoint(x: contentRect.minX + 10, y: contentRect.maxY - 12)
        if scaleSpec == nil || abs(camera.zoom / max(scaleZoom, 1e-9) - 1) > 0.002 {
            scaleZoom = camera.zoom
            let mpp = georef.metersPerPixel(atPixel: camera.center)
            let spec = ScaleBarSpec.make(metersPerPoint: mpp / Double(camera.zoom), maxLength: 110, units: units)
            if let spec, spec != scaleSpec {
                if spec.label != scaleSpec?.label {
                    let image = OverlayArt.pill(spec.label, fontSize: 11, weight: .bold)
                    scaleLabel.contents = image.cgImage
                    scaleLabel.contentsScale = image.scale
                    scaleLabel.bounds = CGRect(origin: .zero, size: image.size)
                }
                let path = CGMutablePath()
                path.move(to: CGPoint(x: 0, y: -7))
                path.addLine(to: .zero)
                path.addLine(to: CGPoint(x: spec.length, y: 0))
                path.addLine(to: CGPoint(x: spec.length, y: -7))
                scaleBarLayer.path = path
            }
            scaleSpec = spec
        }
        let visible = scaleSpec != nil
        scaleBarLayer.isHidden = !visible
        scaleLabel.isHidden = !visible
        if origin != scaleOrigin {
            scaleOrigin = origin
            scaleBarLayer.position = origin
            scaleLabel.position = CGPoint(x: origin.x - 2, y: origin.y - 11)
        }
    }

    private func renderOffscreen(userPoint: CGPoint?, camera: MapCamera, contentRect: CGRect) {
        guard let userPoint, let here = userLatLon, let georef,
              !contentRect.insetBy(dx: -6, dy: -6).contains(userPoint), contentRect.width > 80, contentRect.height > 80
        else {
            offscreenArrow.isHidden = true
            offscreenLabel.isHidden = true
            return
        }
        let box = contentRect.insetBy(dx: 22, dy: 22)
        let c = CGPoint(x: box.midX, y: box.midY)
        let dx = userPoint.x - c.x, dy = userPoint.y - c.y
        let len = max(1e-6, sqrt(dx * dx + dy * dy))
        let tx = abs(dx) > 1e-6 ? (box.width / 2) / abs(dx) : .infinity
        let ty = abs(dy) > 1e-6 ? (box.height / 2) / abs(dy) : .infinity
        let k = min(tx, ty)
        let edge = CGPoint(x: c.x + dx * k, y: c.y + dy * k)
        offscreenArrow.isHidden = false
        offscreenArrow.position = edge
        offscreenArrow.setAffineTransform(CGAffineTransform(rotationAngle: atan2(dx, -dy)))

        let viewCentre = georef.latLon(forPixel: camera.center)
        let text = GeoFormat.distance(Geodesy.distance(viewCentre, here), units: units)
        if text != offscreenText {
            offscreenText = text
            let image = OverlayArt.pill(text, fontSize: 12, weight: .bold, textColor: .white,
                                        background: Theme.uiUserRed.withAlphaComponent(0.92))
            offscreenLabel.contents = image.cgImage
            offscreenLabel.contentsScale = image.scale
            offscreenLabel.bounds = CGRect(origin: .zero, size: image.size)
        }
        let ux = dx / len, uy = dy / len
        let back = 22 + max(offscreenLabel.bounds.width, offscreenLabel.bounds.height) / 2
        offscreenLabel.position = CGPoint(x: edge.x - ux * back, y: edge.y - uy * back)
        offscreenLabel.isHidden = false
    }
}
