import CoreLocation
import QuartzCore
import UIKit

/// Screen-space overlays drawn above the (transformed) map, in the style of Apple Maps: the
/// location puck (white ring, gradient disc, breathing halo, heading "flashlight" beam, accuracy
/// circle), the dotted walked trail, POI glyph markers with halo labels, the selected POI's balloon
/// pin and guide line, calibration pins, the scale bar and the off-screen arrow.
/// Inputs are set when they change; `render` positions everything for a camera and is cheap
/// enough to run every frame (no allocations on the common path, implicit animations disabled).
final class MapOverlayView: UIView {
    var units: DistanceUnits = .metric {
        didSet {
            guard units != oldValue else { return }
            scaleSpec = nil
            rebuildPOIs()
            updateGuideText()
        }
    }

    // MARK: Inputs

    private var georef: Georeference?
    /// Coordinates → map pixels through the georeference (homography and, for photos of folded
    /// paper, its warp), with the inverse worked out once per georeference.
    private var projector: Georeference.Projector?
    private var location: CLLocation?
    /// Where the latest fix is on the map.
    private var targetUserPixel: CGPoint?
    /// Where the puck is drawn: glides to `targetUserPixel` after every fix.
    private var shownUserPixel: CGPoint?
    private var glideFrom: CGPoint = .zero
    private var glideStart: CFTimeInterval = 0
    private var gliding = false
    private var userLatLon: LatLon?
    private var metersPerPixelAtUser = 1.0
    private var northAtUser = 0.0
    /// Accuracy radius in map pixels: target from the fix, and the animated value drawn.
    private var accuracyTargetPx: CGFloat = 0
    private var accuracyShownPx: CGFloat = -1
    private var isStale = false
    private var staleTask: Task<Void, Never>?

    private var puckColor: UIColor = LocationColor.red.uiColor
    /// Colours for the dark map (`MapAppearance.dark`) instead of paper.
    private var darkMap = false

    private var trackPoints: [TrackPoint] = []
    private var trackPixels: [CGPoint] = []
    private var showTrack = true

    private var pois: [MapPOI] = []
    private var items: [POIItem] = []
    private var showPOIs = true
    private var showMapLabels = true
    private var placementDirty = true
    private var selectedID: UUID?

    private var markers: [CanvasMarker] = []
    private var markerLayers: [CALayer] = []

    // MARK: Layers

    private let accuracyLayer = CALayer()
    private let beamLayer = CALayer()
    private let trailHalo = CAShapeLayer()
    private let trailDots = CAShapeLayer()
    private let trailStart = CALayer()
    private let guideCasing = CAShapeLayer()
    private let guideLayer = CAShapeLayer()
    private let poiContainer = CALayer()
    private let balloonLayer = CALayer()
    private let balloonLabel = CALayer()
    private let guideLabel = CALayer()
    private let haloLayer = CALayer()
    private let puckLayer = CALayer()
    private let puckShadow = CALayer()
    private let puckRing = CALayer()
    private let puckDisc = CAGradientLayer()
    private let markerContainer = CALayer()
    /// Features' layers (`CanvasOverlay`), by level: under the trail, above the POI markers,
    /// above everything.
    private let featureRoutesContainer = CALayer()
    private let featureMarkersContainer = CALayer()
    private let featureTopContainer = CALayer()
    /// The puck's halo and disc breathe (render-server animations); the battery saver stops them.
    private var animationsEnabled = true
    private let offscreenArrow = CAShapeLayer()
    private let offscreenLabel = CALayer()
    private let scaleLayer = CALayer()

    // MARK: Render caches

    private var lastCamera: CanvasCamera?
    private var lastPlacementCamera: CanvasCamera?
    /// Trail path in "trail space": view offsets from `trailOrigin` for the zoom and rotation it was built at.
    private var trailPath = CGMutablePath()
    private var trailOrigin: CGPoint = .zero
    private var trailZoom: CGFloat = 0
    private var trailRotation: CGFloat = 0
    /// Last point added to `trailPath` (trail space) and how many track pixels it covers.
    private var trailLast: CGPoint = .zero
    private var trailBuiltCount = 0
    private var trailNeedsRebuild = true
    private var beamWidth = -1
    private var guideText = ""
    private var renderedGuideText = ""
    private var balloonKind: POIKind?
    private var balloonID: UUID?
    private var offscreenText = ""
    private var scaleSpec: ScaleBarSpec?
    private var scaleDark = false
    private var scaleZoom: CGFloat = 0
    private var scaleShown = false
    private var scaleHideAt: CFTimeInterval = 0
    private var scaleTask: Task<Void, Never>?

    /// Puck: inner disc and white ring.
    static let discSize: CGFloat = 15
    static let ringWidth: CGFloat = 3.5
    static var puckSize: CGFloat { discSize + ringWidth * 2 }
    static let beamRadius: CGFloat = 64
    /// Time the puck takes to glide to a new fix.
    static let puckGlide: CFTimeInterval = 0.8
    /// A fix older than this is shown as a grey "last known" puck.
    static let staleAfter: TimeInterval = 30
    /// POI labels show when zoomed in to at least this many points per ground metre⁻¹ (metres per point)…
    static let labelMaxMetersPerPoint = 7.0
    /// …and the trail skips points closer than this on screen.
    static let trailMinSpacing: CGFloat = 3
    /// Dotted trail: dot diameter, halo diameter and dot spacing (points).
    static let trailDot: CGFloat = 7
    static let trailHaloWidth: CGFloat = 10.5
    static let trailSpacing: CGFloat = 12

    private final class POIItem {
        let poi: MapPOI
        let pixel: CGPoint
        let priority: Int
        let labelText: String
        let marker = CALayer()
        let label = CALayer()
        var screen: CGPoint = .zero
        var markerVisible = false
        var labelVisible = false
        /// Label to the left of the marker instead of the right.
        var labelLeft = false
        /// The label bitmap is drawn the first time the label shows.
        var labelReady = false

        init(poi: MapPOI, pixel: CGPoint, labelText: String) {
            self.poi = poi
            self.pixel = pixel
            self.labelText = labelText
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
        if window != nil, animationsEnabled { startBreathing() }
    }

    /// Adds a feature's layer at its level (see `CanvasOverlayLevel`).
    func addFeatureLayer(_ layer: CALayer, level: CanvasOverlayLevel) {
        switch level {
        case .routes: featureRoutesContainer.addSublayer(layer)
        case .markers: featureMarkersContainer.addSublayer(layer)
        case .top: featureTopContainer.addSublayer(layer)
        }
    }

    /// Idle animations on or off (the battery saver turns them off: they keep the render server
    /// drawing even when nothing else moves).
    func setAnimationsEnabled(_ enabled: Bool) {
        guard enabled != animationsEnabled else { return }
        animationsEnabled = enabled
        if enabled {
            if window != nil { startBreathing() }
        } else {
            haloLayer.removeAnimation(forKey: "breathe")
            puckDisc.removeAnimation(forKey: "breathe")
        }
    }

    private func setUpLayers() {
        accuracyLayer.borderWidth = 0.75

        let b = Self.beamRadius * 2
        beamLayer.bounds = CGRect(x: 0, y: 0, width: b, height: b)

        // Zero-length dashes with round caps draw dots; the wider white dots underneath give each a halo.
        for (l, color, width) in [(trailHalo, UIColor.white, Self.trailHaloWidth), (trailDots, Theme.uiTrack, Self.trailDot)] {
            l.fillColor = nil
            l.strokeColor = color.cgColor
            l.lineWidth = width
            l.lineCap = .round
            l.lineJoin = .round
            l.lineDashPattern = [0.001, NSNumber(value: Double(Self.trailSpacing))]
            l.anchorPoint = .zero
            l.bounds = .zero
        }
        let start = OverlayArt.trailStart()
        trailStart.contents = start.cgImage
        trailStart.contentsScale = start.scale
        trailStart.bounds = CGRect(origin: .zero, size: start.size)

        guideCasing.fillColor = nil
        guideCasing.strokeColor = UIColor.white.withAlphaComponent(0.9).cgColor
        guideCasing.lineWidth = 6
        guideCasing.lineCap = .round
        guideCasing.lineDashPattern = [8, 7]
        guideLayer.fillColor = nil
        guideLayer.lineWidth = 3
        guideLayer.lineCap = .round
        guideLayer.lineDashPattern = [8, 7]

        balloonLabel.anchorPoint = CGPoint(x: 0.5, y: 0)

        let s = Self.puckSize
        haloLayer.bounds = CGRect(x: 0, y: 0, width: s, height: s)
        haloLayer.cornerRadius = s / 2

        puckLayer.bounds = CGRect(x: 0, y: 0, width: s, height: s)
        puckShadow.frame = puckLayer.bounds
        puckShadow.shadowPath = UIBezierPath(ovalIn: puckShadow.bounds).cgPath
        puckShadow.shadowColor = UIColor.black.cgColor
        puckShadow.shadowOpacity = 0.35
        puckShadow.shadowRadius = 3
        puckShadow.shadowOffset = CGSize(width: 0, height: 1.5)
        puckRing.frame = puckLayer.bounds
        puckRing.cornerRadius = s / 2
        puckRing.backgroundColor = UIColor.white.cgColor
        let d = Self.discSize
        puckDisc.frame = CGRect(x: (s - d) / 2, y: (s - d) / 2, width: d, height: d)
        puckDisc.cornerRadius = d / 2
        puckDisc.masksToBounds = true
        puckDisc.startPoint = CGPoint(x: 0.5, y: 0)
        puckDisc.endPoint = CGPoint(x: 0.5, y: 1)
        puckLayer.addSublayer(puckShadow)
        puckLayer.addSublayer(puckRing)
        puckLayer.addSublayer(puckDisc)

        offscreenArrow.path = OverlayArt.arrowPath()
        offscreenArrow.strokeColor = UIColor.white.cgColor
        offscreenArrow.lineWidth = 2
        offscreenArrow.lineJoin = .round
        offscreenArrow.shadowColor = UIColor.black.cgColor
        offscreenArrow.shadowOpacity = 0.4
        offscreenArrow.shadowRadius = 4
        offscreenArrow.shadowOffset = CGSize(width: 0, height: 1)
        offscreenArrow.shadowPath = offscreenArrow.path

        scaleLayer.anchorPoint = .zero
        scaleLayer.opacity = 0

        let stack: [CALayer] = [accuracyLayer, beamLayer, featureRoutesContainer, trailHalo, trailDots, trailStart,
                                guideCasing, guideLayer, poiContainer, featureMarkersContainer, haloLayer, puckLayer,
                                balloonLayer, balloonLabel, guideLabel, markerContainer, featureTopContainer,
                                offscreenArrow, offscreenLabel, scaleLayer]
        for l in stack {
            layer.addSublayer(l)
        }
        let initiallyHidden: [CALayer] = [accuracyLayer, beamLayer, trailHalo, trailDots, trailStart, guideCasing, guideLayer,
                                          haloLayer, puckLayer, balloonLayer, balloonLabel, guideLabel,
                                          offscreenArrow, offscreenLabel, scaleLayer]
        for l in initiallyHidden {
            l.isHidden = true
        }
        applyPuckStyle()
    }

    /// Gentle breathing of the halo and the disc (render-server animations, no CPU per frame).
    private func startBreathing() {
        if haloLayer.animation(forKey: "breathe") == nil {
            let scale = CABasicAnimation(keyPath: "transform.scale")
            scale.fromValue = 1.1
            scale.toValue = 1.85
            let fade = CABasicAnimation(keyPath: "opacity")
            fade.fromValue = 0.34
            fade.toValue = 0.06
            let group = CAAnimationGroup()
            group.animations = [scale, fade]
            group.duration = 1.7
            group.autoreverses = true
            group.repeatCount = .infinity
            group.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            group.isRemovedOnCompletion = false
            haloLayer.add(group, forKey: "breathe")
        }
        if puckDisc.animation(forKey: "breathe") == nil {
            let disc = CABasicAnimation(keyPath: "transform.scale")
            disc.fromValue = 1.0
            disc.toValue = 0.86
            disc.duration = 1.7
            disc.autoreverses = true
            disc.repeatCount = .infinity
            disc.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            disc.isRemovedOnCompletion = false
            puckDisc.add(disc, forKey: "breathe")
        }
    }

    /// Colours of the puck, halo, accuracy circle, beam and off-screen arrow (grey when stale).
    private func applyPuckStyle() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let base = isStale ? UIColor(white: 0.6, alpha: 1) : puckColor
        puckDisc.colors = [OverlayArt.mix(base, .white, 0.22).cgColor, OverlayArt.mix(base, .black, 0.1).cgColor]
        haloLayer.backgroundColor = base.cgColor
        // A faint tint vanishes on the dark map: a little more fill and a clearer rim there.
        accuracyLayer.backgroundColor = base.withAlphaComponent(darkMap ? 0.16 : 0.12).cgColor
        accuracyLayer.borderColor = base.withAlphaComponent(darkMap ? 0.4 : 0.22).cgColor
        offscreenArrow.fillColor = base.cgColor
        offscreenText = ""
        beamWidth = -1
        CATransaction.commit()
    }

    // MARK: - Inputs

    func setStyle(locationColor: UIColor, darkMap: Bool) {
        let colorChanged = !locationColor.isEqual(puckColor)
        let mapChanged = darkMap != self.darkMap
        puckColor = locationColor
        self.darkMap = darkMap
        if colorChanged || mapChanged { applyPuckStyle() }
        if mapChanged {
            rebuildPOIs()
            scaleSpec = nil
            renderedGuideText = ""
        }
    }

    func setGeoref(_ g: Georeference?) {
        guard g != georef else { return }
        georef = g
        projector = g.map { Georeference.Projector($0) }
        recomputeUser()
        shownUserPixel = targetUserPixel
        gliding = false
        accuracyShownPx = -1
        trackPixels = []
        trailNeedsRebuild = true
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
        if let target = targetUserPixel {
            var jump = Double.infinity
            if let shown = shownUserPixel {
                let pixels = Double(hypot(shown.x - target.x, shown.y - target.y))
                jump = pixels * metersPerPixelAtUser
            }
            if let shown = shownUserPixel, jump < 2000 {
                glideFrom = shown
                glideStart = CACurrentMediaTime()
                gliding = true
            } else {
                shownUserPixel = target
                gliding = false
            }
        } else {
            shownUserPixel = nil
            gliding = false
        }
        updateStale()
    }

    /// Grey "last known position" puck once the fix is older than `staleAfter`.
    private func updateStale() {
        staleTask?.cancel()
        staleTask = nil
        guard let fix = location else { return }
        let age = Date().timeIntervalSince(fix.timestamp)
        let stale = age > Self.staleAfter
        if stale != isStale {
            isStale = stale
            applyPuckStyle()
        }
        guard !stale else { return }
        let delay = max(0.1, Self.staleAfter - age + 0.25)
        staleTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard let self, !Task.isCancelled else { return }
            self.isStale = true
            self.applyPuckStyle()
            self.haloLayer.isHidden = true
            self.beamLayer.isHidden = true
        }
    }

    func setTrack(_ points: [TrackPoint], visible: Bool) {
        if visible != showTrack {
            showTrack = visible
            trailNeedsRebuild = true
        }
        if points.count == trackPoints.count, points.first == trackPoints.first, points.last == trackPoints.last { return }
        let appended = points.count > trackPoints.count && !trackPoints.isEmpty
            && points.first == trackPoints.first && points[trackPoints.count - 1] == trackPoints.last
        let start = appended ? trackPoints.count : 0
        trackPoints = points
        if !appended {
            trackPixels = []
            trailNeedsRebuild = true
        }
        convertTrack(from: start)
    }

    func setPOIs(_ list: [MapPOI], visible: Bool, showMapLabels: Bool) {
        if visible != showPOIs || showMapLabels != self.showMapLabels {
            showPOIs = visible
            self.showMapLabels = showMapLabels
            poiContainer.isHidden = !visible
            placementDirty = true
        }
        guard list != pois else { return }
        pois = list
        rebuildPOIs()
    }

    func setSelected(_ id: UUID?) {
        guard id != selectedID else { return }
        selectedID = id
        placementDirty = true
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
        for item in items where item.markerVisible || item.poi.id == selectedID {
            // The balloon's head sits above the POI.
            let isSelected = item.poi.id == selectedID
            let lift: CGFloat = isSelected ? OverlayArt.balloonHeight * 0.6 : 0
            let y = item.screen.y - lift
            let dx = item.screen.x - point.x, dy = y - point.y
            let d2 = dx * dx + dy * dy
            if d2 < 24 * 24, d2 < (best?.1 ?? .infinity) { best = (item, d2) }
        }
        return best?.0.poi
    }

    /// Map pixel of a POI as currently placed (nil if not on this map's georeference).
    func pixel(ofPOI id: UUID) -> CGPoint? { items.first { $0.poi.id == id }?.pixel }

    /// Where the puck is drawn (it glides between fixes), in map pixels.
    var userMapPixel: CGPoint? { shownUserPixel }
    var userMetersPerPixel: Double { metersPerPixelAtUser }
    var userNorthAngle: Double { northAtUser }

    /// Show the scale bar now and fade it out about 1.5 s after the last call (zooming or panning).
    func flashScaleBar() {
        guard georef != nil else { return }
        scaleHideAt = CACurrentMediaTime() + 1.5
        if !scaleShown {
            scaleShown = true
            fadeScale(to: 1, duration: 0.18)
        }
        if scaleTask == nil { scheduleScaleFade(after: 1.5) }
    }

    /// One pending task at a time: it re-arms itself while the map keeps moving.
    private func scheduleScaleFade(after delay: Double) {
        scaleTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0.05, delay) * 1_000_000_000))
            guard let self, !Task.isCancelled else { return }
            self.scaleTask = nil
            let remaining = self.scaleHideAt - CACurrentMediaTime()
            if remaining > 0.05 {
                self.scheduleScaleFade(after: remaining)
            } else {
                self.scaleShown = false
                self.fadeScale(to: 0, duration: 0.45)
            }
        }
    }

    private func fadeScale(to value: Float, duration: CFTimeInterval) {
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = scaleLayer.presentation()?.opacity ?? scaleLayer.opacity
        fade.toValue = value
        fade.duration = duration
        fade.timingFunction = CAMediaTimingFunction(name: .easeOut)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        scaleLayer.opacity = value
        CATransaction.commit()
        scaleLayer.add(fade, forKey: "fade")
    }

    // MARK: - Animation

    /// Advances the puck glide and the accuracy radius. Returns true while something still moves.
    func advance(now: CFTimeInterval, dt: Double) -> Bool {
        var active = false
        if gliding, let target = targetUserPixel {
            let t = min(1, max(0, (now - glideStart) / Self.puckGlide))
            // Ease out: quick to follow the fix, gentle when arriving.
            let e = CGFloat(1 - (1 - t) * (1 - t))
            shownUserPixel = CGPoint(x: glideFrom.x + (target.x - glideFrom.x) * e,
                                     y: glideFrom.y + (target.y - glideFrom.y) * e)
            if t >= 1 {
                gliding = false
                shownUserPixel = target
            } else {
                active = true
            }
        }
        if accuracyShownPx < 0 {
            accuracyShownPx = accuracyTargetPx
        } else if abs(accuracyShownPx - accuracyTargetPx) > max(0.05, accuracyTargetPx * 0.003) {
            accuracyShownPx += (accuracyTargetPx - accuracyShownPx) * CGFloat(1 - exp(-dt / 0.25))
            active = true
        } else {
            accuracyShownPx = accuracyTargetPx
        }
        return active
    }

    // MARK: - Derived data

    private func recomputeUser() {
        guard let georef, let projector, let fix = location else {
            targetUserPixel = nil
            userLatLon = nil
            updateGuideText()
            return
        }
        let ll = LatLon(fix.coordinate)
        let p = projector.pixel(for: ll)
        guard p.x.isFinite, p.y.isFinite else {
            targetUserPixel = nil
            userLatLon = nil
            return
        }
        targetUserPixel = p
        userLatLon = ll
        let mpp = georef.metersPerPixel(atPixel: p)
        metersPerPixelAtUser = mpp.isFinite && mpp > 0 ? mpp : 1
        northAtUser = georef.northAngle(atPixel: p)
        accuracyTargetPx = CGFloat(max(0, fix.horizontalAccuracy) / metersPerPixelAtUser)
        updateGuideText()
    }

    private func convertTrack(from start: Int) {
        guard let projector, start < trackPoints.count else { return }
        trackPixels.reserveCapacity(trackPoints.count)
        for i in start..<trackPoints.count {
            trackPixels.append(projector.pixel(for: trackPoints[i].latLon))
        }
    }

    private func rebuildPOIs() {
        items.forEach {
            $0.marker.removeFromSuperlayer()
            $0.label.removeFromSuperlayer()
        }
        items = []
        placementDirty = true
        balloonKind = nil
        balloonID = nil
        guard let projector else { return }
        var built: [POIItem] = []
        built.reserveCapacity(pois.count)
        for poi in pois {
            let pixel = projector.pixel(for: poi.position)
            guard pixel.x.isFinite, pixel.y.isFinite else { continue }
            let text = Self.labelText(for: poi, units: units)
            let item = POIItem(poi: poi, pixel: pixel, labelText: text)
            let marker = OverlayArt.marker(for: poi.kind)
            item.marker.contents = marker.cgImage
            item.marker.contentsScale = marker.scale
            item.marker.bounds = CGRect(origin: .zero, size: marker.size)
            item.marker.isHidden = true
            item.label.bounds = CGRect(origin: .zero, size: OverlayArt.haloTextSize(text))
            item.label.isHidden = true
            poiContainer.addSublayer(item.label)
            poiContainer.addSublayer(item.marker)
            built.append(item)
        }
        // Higher priority first, so it wins collisions.
        items = built.sorted { $0.priority < $1.priority }
        updateGuideText()
    }

    private static func labelText(for poi: MapPOI, units: DistanceUnits) -> String {
        var text = poi.name.isEmpty ? POIKindStyle.name(for: poi.kind) : poi.name
        if let ele = poi.elevationM, ele.isFinite, !text.contains(String(Int(ele.rounded()))) {
            text += " · " + GeoFormat.elevation(ele, units: units)
        }
        return text
    }

    /// Category-coloured label text, darkened on paper and lightened on the dark map for contrast.
    private static func labelColor(for kind: POIKind, darkMap: Bool) -> UIColor {
        let c = POIKindStyle.uiColor(for: kind)
        return darkMap ? OverlayArt.mix(c, .white, 0.45) : OverlayArt.mix(c, .black, 0.35)
    }

    /// Halo behind label text: white on paper; on the dark map a near-black a shade below its
    /// paper, so labels lift off light ink (grid lines, tracks) as on Apple's dark maps.
    private static func labelHalo(darkMap: Bool) -> UIColor {
        darkMap ? UIColor(red: 0.05, green: 0.05, blue: 0.06, alpha: 0.88) : UIColor(white: 1, alpha: 0.95)
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

    private func isShown(_ item: POIItem) -> Bool {
        item.poi.source != .mapLabel || showMapLabels || item.poi.id == selectedID
    }

    // MARK: - Render

    /// Position every overlay for `camera`. `heading` is the smoothed true heading in degrees.
    func render(camera: CanvasCamera, contentRect: CGRect, heading: Double?, headingAccuracy: Double?) {
        let size = bounds.size
        guard size.width > 0, size.height > 0 else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let t = camera.transform(viewSize: size)
        let userPoint = shownUserPixel.map { $0.applying(t) }

        renderTrail(camera: camera, viewSize: size)
        renderUser(at: userPoint, camera: camera, heading: heading, headingAccuracy: headingAccuracy)
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

    private func renderUser(at point: CGPoint?, camera: CanvasCamera, heading: Double?, headingAccuracy: Double?) {
        guard let point, location != nil else {
            puckLayer.isHidden = true
            haloLayer.isHidden = true
            accuracyLayer.isHidden = true
            beamLayer.isHidden = true
            return
        }
        puckLayer.isHidden = false
        puckLayer.position = point
        haloLayer.isHidden = isStale
        haloLayer.position = point

        let radius = max(0, accuracyShownPx) * camera.zoom
        if radius > Self.puckSize / 2 + 2, radius < 4000 {
            accuracyLayer.isHidden = false
            accuracyLayer.bounds = CGRect(x: 0, y: 0, width: radius * 2, height: radius * 2)
            accuracyLayer.cornerRadius = radius
            accuracyLayer.position = point
        } else {
            accuracyLayer.isHidden = true
        }

        guard let heading, !isStale else {
            beamLayer.isHidden = true
            return
        }
        // Narrower with a better compass: 60° at ±10° or better, 75° at ±40° or worse.
        let accuracy = headingAccuracy ?? 25
        let spread = min(max((accuracy - 10) / 30, 0), 1)
        let width = 60 + Int((spread * 3).rounded()) * 5
        if width != beamWidth {
            beamWidth = width
            let image = OverlayArt.beam(color: puckColor, widthDegrees: width, radius: Self.beamRadius)
            beamLayer.contents = image.cgImage
            beamLayer.contentsScale = image.scale
        }
        let angle = MapAngle.radians(heading) + northAtUser + Double(camera.rotation)
        beamLayer.isHidden = false
        beamLayer.position = point
        beamLayer.setAffineTransform(CGAffineTransform(rotationAngle: CGFloat(angle)))
    }

    /// The dotted trail. The path is built in screen space for the current zoom and only moved
    /// (panning) or turned (rotation: dots are round, so turning the layer is exact) otherwise; new
    /// points are appended without rebuilding.
    private func renderTrail(camera: CanvasCamera, viewSize: CGSize) {
        guard showTrack, georef != nil, trackPixels.count > 1 else {
            if !trailDots.isHidden || trailDots.path != nil {
                trailDots.isHidden = true
                trailHalo.isHidden = true
                trailStart.isHidden = true
                trailDots.path = nil
                trailHalo.path = nil
                trailBuiltCount = 0
            }
            return
        }
        let zoomChanged = abs(camera.zoom / max(trailZoom, 1e-9) - 1) > 1e-4
        let stale = trailNeedsRebuild || trailBuiltCount == 0 || trailBuiltCount > trackPixels.count
        if stale || zoomChanged {
            trailNeedsRebuild = false
            trailZoom = camera.zoom
            trailRotation = camera.rotation
            trailOrigin = camera.center
            trailPath = CGMutablePath()
            trailLast = trailSpace(trackPixels[0], camera: camera)
            trailPath.move(to: trailLast)
            trailBuiltCount = 1
        }
        if trailBuiltCount < trackPixels.count {
            let minD2 = Self.trailMinSpacing * Self.trailMinSpacing
            let lastIndex = trackPixels.count - 1
            for i in trailBuiltCount...lastIndex {
                let v = trailSpace(trackPixels[i], camera: camera)
                let dx = v.x - trailLast.x, dy = v.y - trailLast.y
                // Always end exactly at the newest point; later appends continue from there.
                if dx * dx + dy * dy < minD2, i != lastIndex { continue }
                trailPath.addLine(to: v)
                trailLast = v
            }
            trailBuiltCount = trackPixels.count
            let path = trailPath.copy()
            trailDots.path = path
            trailHalo.path = path
        }
        let origin = camera.viewPoint(forMap: trailOrigin, viewSize: viewSize)
        let turn = CGAffineTransform(rotationAngle: camera.rotation - trailRotation)
        trailDots.position = origin
        trailHalo.position = origin
        trailDots.setAffineTransform(turn)
        trailHalo.setAffineTransform(turn)
        trailDots.isHidden = false
        trailHalo.isHidden = false
        trailStart.isHidden = false
        trailStart.position = camera.viewPoint(forMap: trackPixels[0], viewSize: viewSize)
    }

    /// A map pixel in trail space (view offset from `trailOrigin` at the trail's zoom and rotation).
    private func trailSpace(_ p: CGPoint, camera: CanvasCamera) -> CGPoint {
        let dx = (p.x - trailOrigin.x) * trailZoom
        let dy = (p.y - trailOrigin.y) * trailZoom
        let c = cos(trailRotation), s = sin(trailRotation)
        return CGPoint(x: dx * c - dy * s, y: dx * s + dy * c)
    }

    private func renderPOIs(_ t: CGAffineTransform, camera: CanvasCamera) {
        guard showPOIs, !items.isEmpty else {
            balloonLayer.isHidden = true
            balloonLabel.isHidden = true
            return
        }
        for item in items {
            item.screen = item.pixel.applying(t)
        }
        if placementDirty || placementCameraMoved(camera) {
            place(camera: camera)
        }
        let visibleRect = bounds.insetBy(dx: -40, dy: -40)
        for item in items {
            let onScreen = visibleRect.contains(item.screen)
            let showMarker = item.markerVisible && onScreen
            item.marker.isHidden = !showMarker
            if showMarker { item.marker.position = item.screen }
            let showLabel = item.labelVisible && onScreen
            item.label.isHidden = !showLabel
            if showLabel {
                if !item.labelReady { drawLabel(item) }
                let w = item.label.bounds.width
                let dx = OverlayArt.markerSize / 2 + 1 + w / 2
                item.label.position = CGPoint(x: item.screen.x + (item.labelLeft ? -dx : dx), y: item.screen.y)
            }
        }
    }

    private func drawLabel(_ item: POIItem) {
        item.labelReady = true
        let image = OverlayArt.haloText(item.labelText, color: Self.labelColor(for: item.poi.kind, darkMap: darkMap),
                                        halo: Self.labelHalo(darkMap: darkMap))
        item.label.contents = image.cgImage
        item.label.contentsScale = image.scale
        item.label.bounds = CGRect(origin: .zero, size: image.size)
    }

    private func placementCameraMoved(_ camera: CanvasCamera) -> Bool {
        guard let last = lastPlacementCamera else { return true }
        return abs(camera.zoom / last.zoom - 1) > 0.04 || abs(MapAngle.normalized(Double(camera.rotation - last.rotation))) > 0.04
    }

    /// Greedy placement: higher-priority markers first, skipping any that would cover one already
    /// placed; labels (zoomed in only) to the right of their marker, else to the left, else hidden.
    /// Translation never changes overlaps, so this only reruns on zoom/rotation.
    private func place(camera: CanvasCamera) {
        placementDirty = false
        lastPlacementCamera = camera
        let mpp = georef?.metersPerPixel(atPixel: camera.center) ?? metersPerPixelAtUser
        let metersPerPoint = mpp / Double(max(camera.zoom, 1e-9))
        let labelsAllowed = metersPerPoint <= Self.labelMaxMetersPerPoint
        let m = OverlayArt.markerSize
        var taken: [CGRect] = []
        taken.reserveCapacity(items.count * 2)
        // The selected POI is drawn as a balloon pin; keep its spot free.
        if let id = selectedID, let sel = items.first(where: { $0.poi.id == id }) {
            let h = OverlayArt.balloonHeight
            taken.append(CGRect(x: sel.screen.x - h * 0.4, y: sel.screen.y - h, width: h * 0.8, height: h))
        }
        for item in items {
            item.markerVisible = false
            item.labelVisible = false
            guard isShown(item), item.poi.id != selectedID else { continue }
            let rect = CGRect(x: item.screen.x - m / 2, y: item.screen.y - m / 2, width: m, height: m).insetBy(dx: -2, dy: -2)
            if taken.contains(where: { $0.intersects(rect) }) { continue }
            taken.append(rect)
            item.markerVisible = true
            guard labelsAllowed else { continue }
            let size = item.label.bounds.size
            let right = CGRect(x: item.screen.x + m / 2 + 1, y: item.screen.y - size.height / 2,
                               width: size.width, height: size.height).insetBy(dx: 2, dy: 2)
            let left = right.offsetBy(dx: -(size.width + m + 2), dy: 0)
            if !taken.contains(where: { $0.intersects(right) }) {
                taken.append(right)
                item.labelVisible = true
                item.labelLeft = false
            } else if !taken.contains(where: { $0.intersects(left) }) {
                taken.append(left)
                item.labelVisible = true
                item.labelLeft = true
            }
        }
    }

    private func renderSelection(userPoint: CGPoint?) {
        guard let id = selectedID, showPOIs, let item = items.first(where: { $0.poi.id == id }) else {
            balloonLayer.isHidden = true
            balloonLabel.isHidden = true
            guideLayer.isHidden = true
            guideCasing.isHidden = true
            guideLabel.isHidden = true
            return
        }
        if balloonKind != item.poi.kind {
            balloonKind = item.poi.kind
            let image = OverlayArt.balloon(for: item.poi.kind)
            balloonLayer.contents = image.cgImage
            balloonLayer.contentsScale = image.scale
            balloonLayer.bounds = CGRect(origin: .zero, size: image.size)
            balloonLayer.anchorPoint = CGPoint(x: 0.5, y: (image.size.height - 1) / image.size.height)
            guideLayer.strokeColor = POIKindStyle.uiColor(for: item.poi.kind).cgColor
        }
        if balloonID != id {
            balloonID = id
            let text = item.labelText
            let ink = darkMap ? UIColor.white : UIColor(white: 0.08, alpha: 1)
            let image = OverlayArt.haloText(text, color: ink, halo: Self.labelHalo(darkMap: darkMap), fontSize: 13, weight: .bold)
            balloonLabel.contents = image.cgImage
            balloonLabel.contentsScale = image.scale
            balloonLabel.bounds = CGRect(origin: .zero, size: image.size)
        }
        balloonLayer.isHidden = false
        balloonLayer.position = item.screen
        balloonLabel.isHidden = false
        balloonLabel.position = CGPoint(x: item.screen.x, y: item.screen.y + 3)

        guard let userPoint, !guideText.isEmpty else {
            guideLayer.isHidden = true
            guideCasing.isHidden = true
            guideLabel.isHidden = true
            return
        }
        let path = CGMutablePath()
        path.move(to: userPoint)
        path.addLine(to: item.screen)
        guideLayer.path = path
        guideCasing.path = path
        guideLayer.isHidden = false
        guideCasing.isHidden = false
        if guideText != renderedGuideText {
            renderedGuideText = guideText
            // On the dark map a raised grey pill (Apple's dark elevated fill) stands off the paper.
            let pillColor = darkMap ? UIColor(red: 0.23, green: 0.23, blue: 0.24, alpha: 0.95) : UIColor(white: 0.12, alpha: 0.92)
            let image = OverlayArt.pill(guideText, fontSize: 13, weight: .bold, textColor: .white, background: pillColor)
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

    private func renderScaleBar(camera: CanvasCamera, contentRect: CGRect) {
        guard let georef else {
            scaleLayer.isHidden = true
            return
        }
        if scaleSpec == nil || scaleDark != darkMap || abs(camera.zoom / max(scaleZoom, 1e-9) - 1) > 0.002 {
            scaleZoom = camera.zoom
            let mpp = georef.metersPerPixel(atPixel: camera.center)
            let spec = ScaleBarSpec.make(metersPerPoint: mpp / Double(camera.zoom), maxLength: 96, units: units)
            if let spec, spec != scaleSpec || scaleDark != darkMap {
                scaleDark = darkMap
                let image = OverlayArt.scaleBar(spec, dark: darkMap)
                scaleLayer.contents = image.cgImage
                scaleLayer.contentsScale = image.scale
                scaleLayer.bounds = CGRect(origin: .zero, size: image.size)
            }
            scaleSpec = spec
        }
        scaleLayer.isHidden = scaleSpec == nil
        scaleLayer.position = CGPoint(x: contentRect.minX + 12 - OverlayArt.scaleInset, y: contentRect.minY + 2)
    }

    private func renderOffscreen(userPoint: CGPoint?, camera: CanvasCamera, contentRect: CGRect) {
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
            let base = isStale ? UIColor(white: 0.45, alpha: 1) : puckColor
            let image = OverlayArt.pill(text, fontSize: 12, weight: .bold, textColor: .white,
                                        background: base.withAlphaComponent(0.92))
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
