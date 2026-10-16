import CoreLocation
import QuartzCore
import UIKit

// The maps feature's content on the map canvas (docs/design/explorer.md §7.3): the sheet's edge,
// highlighted once you have walked off it, and the places your other maps know about.

/// The sheet's edge, a hairline where the terrain beyond it shows.
///
/// Off the sheet the edge is drawn clearly — a 2.5 pt accent line with a soft glow — so the
/// border you crossed stands out against the terrain continuing the map; on the sheet a 1 pt
/// hairline (`Theme.uiSheetEdge`, darker on paper) marks where the map ends and the terrain begins.
@MainActor
final class MapEdgeOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.routes
    let layer = CALayer()
    private let glow = CAShapeLayer()
    private let edge = CAShapeLayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?
    private var mapContext: CanvasMapContext?
    private var loop: ObservationLoop?
    /// The sheet's corners in map pixels (TL, TR, BR, BL).
    private var corners: [CGPoint] = []
    private var hasContinuation = false
    private var offSheet = false
    private var darkMap = true
    private var styledOffSheet: Bool?
    private var styledDark: Bool?

    static let hairline: CGFloat = 1
    static let highlight: CGFloat = 2.5

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        for l in [glow, edge] {
            l.fillColor = nil
            l.lineJoin = .miter
            l.isHidden = true
            layer.addSublayer(l)
        }
        glow.shadowOpacity = 0.9
        glow.shadowRadius = 6
        glow.shadowOffset = .zero
        loop = ObservationLoop { [weak self] in
            self?.locationChanged()
        }
    }

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        let s = context.map.pixelSize
        corners = [.zero, CGPoint(x: s.width, y: 0), CGPoint(x: s.width, y: s.height), CGPoint(x: 0, y: s.height)]
        hasContinuation = context.continuationMargin > 0 && context.georef != nil
        darkMap = context.darkMap
        locationChanged()
        context.requestFrame()
    }

    /// Reads the fix (the loop runs this again when it changes).
    private func locationChanged() {
        guard let model else { return }
        let fix = model.location.location
        guard let c = mapContext else { return }
        let whereabouts = MapWhereabouts.of(c.map, fix: fix, continuation: c.continuationMargin)
        let off = whereabouts.map { !$0.isOnSheet } ?? false
        guard off != offSheet else { return }
        offSheet = off
        c.requestFrame()
    }

    private func applyStyle() {
        guard styledOffSheet != offSheet || styledDark != darkMap else { return }
        styledOffSheet = offSheet
        styledDark = darkMap
        if offSheet {
            edge.strokeColor = Theme.uiAccent.cgColor
            edge.lineWidth = Self.highlight
            glow.strokeColor = Theme.uiAccent.withAlphaComponent(0.55).cgColor
            glow.shadowColor = Theme.uiAccent.cgColor
            glow.lineWidth = Self.highlight + 3
        } else {
            edge.strokeColor = (darkMap ? Theme.uiSheetEdge : Theme.uiSheetEdgePaper).cgColor
            edge.lineWidth = Self.hairline
        }
        glow.isHidden = !offSheet
    }

    func render(_ frame: CanvasFrame) {
        guard !corners.isEmpty, mapContext?.georef != nil, hasContinuation || offSheet else {
            edge.isHidden = true
            glow.isHidden = true
            return
        }
        applyStyle()
        // Four corners: a path this small costs nothing to rebuild per frame, and stroking it in
        // view space keeps the line exactly 1 pt at any zoom.
        let path = CGMutablePath()
        path.move(to: frame.viewPoint(forMap: corners[0]))
        for p in corners.dropFirst() {
            path.addLine(to: frame.viewPoint(forMap: p))
        }
        path.closeSubpath()
        edge.path = path
        edge.isHidden = false
        if offSheet {
            glow.path = path
            glow.isHidden = false
        }
    }
}

/// The places your other maps read off their sheets (and their OpenStreetMap places), on this map
/// and the terrain beyond it.
///
/// docs/design/explorer.md §7.3: POIs of your other located maps that fall in this map's drawable
/// rect (sheet + continuation), deduplicated against this map's POIs and each other (the same
/// name within 150 m), drawn like POIs at 80 % opacity and never on top of this map's own
/// markers; names show when zoomed in (≤ 7 m/pt). Another map's labels (names printed on that
/// sheet) show beyond this sheet only, unless "Map labels as markers" is on: on the sheet the
/// name is printed already. Tap → the POI card.
@MainActor
final class OtherMapsPOIOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.markers
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?
    private var mapContext: CanvasMapContext?
    private var loop: ObservationLoop?
    private var items: [Item] = []
    /// This map's own shown POIs (map pixels): other maps' markers keep clear of them.
    private var ownPixels: [CGPoint] = []
    private var signature: Int?
    private var placementDirty = true
    private var lastPlacement: (zoom: CGFloat, rotation: CGFloat)?

    static let opacity: Float = 0.8
    static let labelMaxMetersPerPoint = 7.0
    /// Finger reach for a tap.
    static let hitRadius: CGFloat = 22

    private final class Item {
        let poi: MapPOI
        let pixel: CGPoint
        let priority: Int
        let labelText: String
        let marker = CALayer()
        let label = CALayer()
        var screen: CGPoint = .zero
        var markerVisible = false
        var labelVisible = false
        var labelLeft = false
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

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        layer.opacity = Self.opacity
        // The library (other maps' POIs, maps located or deleted) and the display settings.
        loop = ObservationLoop { [weak self] in
            self?.dataChanged()
        }
    }

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        signature = nil
        dataChanged()
    }

    private func dataChanged() {
        guard let model else { return }
        let maps = model.library.maps
        let settings = model.settings
        let enabled = settings.showOtherMapsPOIs && settings.showPOIs
        let showLabels = settings.showMapLabels
        let units = settings.units
        guard let c = mapContext else { return }
        var hasher = Hasher()
        hasher.combine(enabled)
        hasher.combine(showLabels)
        hasher.combine(units)
        hasher.combine(c.map.id)
        hasher.combine(c.georef)
        hasher.combine(c.continuationMargin)
        hasher.combine(c.darkMap)
        if enabled {
            for map in maps where map.isGeoreferenced {
                hasher.combine(map.id)
                hasher.combine(map.name)
                hasher.combine(map.pois)
            }
        }
        let sig = hasher.finalize()
        guard sig != signature else { return }
        signature = sig
        rebuild(maps: maps, context: c, enabled: enabled, showLabels: showLabels, units: units)
        c.requestFrame()
    }

    private func rebuild(maps: [MapDocument], context c: CanvasMapContext, enabled: Bool, showLabels: Bool,
                         units: DistanceUnits) {
        for item in items {
            item.marker.removeFromSuperlayer()
            item.label.removeFromSuperlayer()
        }
        items = []
        ownPixels = []
        placementDirty = true
        guard enabled, c.projector != nil else { return }
        // This map's POIs as saved now (OpenStreetMap places arrive after it was shown).
        let own = maps.first(where: { $0.id == c.map.id })?.pois ?? c.map.pois
        ownPixels = own.filter { $0.source != .mapLabel || showLabels }.compactMap { c.pixel(for: $0.position) }
        let picked = Self.pick(from: maps, own: own, for: c, showLabels: showLabels)
        var built: [Item] = []
        built.reserveCapacity(picked.count)
        for (poi, pixel) in picked {
            let item = Item(poi: poi, pixel: pixel, labelText: Self.labelText(for: poi, units: units))
            let marker = OverlayArt.marker(for: poi.kind)
            item.marker.contents = marker.cgImage
            item.marker.contentsScale = marker.scale
            item.marker.bounds = CGRect(origin: .zero, size: marker.size)
            item.marker.isHidden = true
            item.label.bounds = CGRect(origin: .zero, size: OverlayArt.haloTextSize(item.labelText))
            item.label.isHidden = true
            layer.addSublayer(item.label)
            layer.addSublayer(item.marker)
            built.append(item)
        }
        items = built.sorted { $0.priority < $1.priority }
    }

    /// The other maps' POIs to draw on `c`'s map, with their pixels: inside the drawable rect, not
    /// the same as one of this map's or one picked before.
    static func pick(from maps: [MapDocument], own: [MapPOI], for c: CanvasMapContext,
                     showLabels: Bool) -> [(MapPOI, CGPoint)] {
        let drawable = c.drawableRect
        let sheet = c.sheetRect
        var seen = DuplicateIndex()
        for poi in own { seen.insert(poi) }
        var out: [(MapPOI, CGPoint)] = []
        for map in maps where map.id != c.map.id && map.isGeoreferenced {
            for poi in map.pois where poi.source != .user {
                guard let pixel = c.pixel(for: poi.position), drawable.contains(pixel) else { continue }
                // A name read off another sheet is printed on this one too, where they overlap.
                if poi.source == .mapLabel, !showLabels, sheet.contains(pixel) { continue }
                guard !seen.contains(poi) else { continue }
                seen.insert(poi)
                out.append((poi, pixel))
            }
        }
        return out
    }

    /// POIs by normalised name (and unnamed ones by kind), for "the same place within 150 m".
    struct DuplicateIndex {
        /// Same name within this distance: the same place.
        static let radiusM = 150.0
        /// Unnamed POIs of one kind within this distance: the same.
        static let unnamedRadiusM = 40.0

        private var named: [String: [LatLon]] = [:]
        private var unnamed: [POIKind: [LatLon]] = [:]

        static func key(_ name: String) -> String {
            name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }

        mutating func insert(_ poi: MapPOI) {
            let k = Self.key(poi.name)
            if k.isEmpty {
                unnamed[poi.kind, default: []].append(poi.position)
            } else {
                named[k, default: []].append(poi.position)
            }
        }

        func contains(_ poi: MapPOI) -> Bool {
            let k = Self.key(poi.name)
            if k.isEmpty {
                return (unnamed[poi.kind] ?? []).contains {
                    Geodesy.distance($0, poi.position) <= Self.unnamedRadiusM
                }
            }
            return (named[k] ?? []).contains { Geodesy.distance($0, poi.position) <= Self.radiusM }
        }
    }

    private static func labelText(for poi: MapPOI, units: DistanceUnits) -> String {
        var text = poi.name.isEmpty ? POIKindStyle.name(for: poi.kind) : poi.name
        if let ele = poi.elevationM, ele.isFinite, !text.contains(String(Int(ele.rounded()))) {
            text += " · " + GeoFormat.elevation(ele, units: units)
        }
        return text
    }

    func render(_ frame: CanvasFrame) {
        guard !items.isEmpty else { return }
        let t = frame.transform
        for item in items {
            item.screen = item.pixel.applying(t)
        }
        let camera = frame.camera
        if placementDirty || placementMoved(camera) {
            place(frame)
        }
        let visible = CGRect(origin: .zero, size: frame.viewSize).insetBy(dx: -40, dy: -40)
        for item in items {
            let onScreen = visible.contains(item.screen)
            let showMarker = item.markerVisible && onScreen
            item.marker.isHidden = !showMarker
            if showMarker { item.marker.position = item.screen }
            let showLabel = item.labelVisible && onScreen
            item.label.isHidden = !showLabel
            if showLabel {
                if !item.labelReady { drawLabel(item) }
                let dx = OverlayArt.markerSize / 2 + 1 + item.label.bounds.width / 2
                item.label.position = CGPoint(x: item.screen.x + (item.labelLeft ? -dx : dx), y: item.screen.y)
            }
        }
    }

    private func placementMoved(_ camera: CanvasCamera) -> Bool {
        guard let last = lastPlacement else { return true }
        return abs(camera.zoom / last.zoom - 1) > 0.04
            || abs(MapAngle.normalized(Double(camera.rotation - last.rotation))) > 0.04
    }

    /// Greedy placement (as the map's own POIs): higher priority first, never over one of this
    /// map's markers or another placed marker; labels right of the marker, else left, else none.
    /// Panning never changes overlaps, so this runs on zoom and rotation changes only.
    private func place(_ frame: CanvasFrame) {
        placementDirty = false
        lastPlacement = (frame.camera.zoom, frame.camera.rotation)
        let labelsAllowed = frame.metersPerPoint > 0 && frame.metersPerPoint <= Self.labelMaxMetersPerPoint
        let m = OverlayArt.markerSize
        var taken: [CGRect] = []
        taken.reserveCapacity(ownPixels.count + items.count * 2)
        for p in ownPixels {
            let s = p.applying(frame.transform)
            taken.append(CGRect(x: s.x - m / 2, y: s.y - m / 2, width: m, height: m).insetBy(dx: -2, dy: -2))
        }
        for item in items {
            item.markerVisible = false
            item.labelVisible = false
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

    private func drawLabel(_ item: Item) {
        item.labelReady = true
        let dark = mapContext?.darkMap ?? true
        let base = POIKindStyle.uiColor(for: item.poi.kind)
        let color = dark ? OverlayArt.mix(base, .white, 0.45) : OverlayArt.mix(base, .black, 0.35)
        let halo = dark ? UIColor(red: 0.05, green: 0.05, blue: 0.06, alpha: 0.88) : UIColor(white: 1, alpha: 0.95)
        let image = OverlayArt.haloText(item.labelText, color: color, halo: halo)
        item.label.contents = image.cgImage
        item.label.contentsScale = image.scale
        item.label.bounds = CGRect(origin: .zero, size: image.size)
    }

    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit? {
        var best: CanvasHit?
        for item in items where item.markerVisible {
            let d = hypot(item.screen.x - point.x, item.screen.y - point.y)
            if d <= Self.hitRadius, d < (best?.distance ?? .infinity) {
                best = CanvasHit(item: nil, poi: item.poi, distance: d)
            }
        }
        return best
    }
}
