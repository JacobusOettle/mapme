import CoreGraphics
import CoreLocation
import Foundation
import Observation

/// How well one map covers a position.
struct MapCoverage: Equatable, Sendable {
    var mapID: UUID
    /// Ground metres per map pixel at the position (smaller = more detailed).
    var metersPerPixel: Double
    /// Distance from the position to the sheet's nearest edge, ground metres (negative outside).
    var insideMarginM: Double
}

/// Why auto-switching picked another map.
enum MapSwitchReason: Equatable, Sendable {
    /// You walked off the map shown and another map covers you.
    case walkedOff
    /// Another map is at least 1.5× more detailed where you are.
    case moreDetailed
}

/// A map to switch to now, and why.
struct MapSwitchDecision: Equatable, Sendable {
    var mapID: UUID
    var reason: MapSwitchReason
}

/// The auto-switching rules (docs/design/explorer.md §7.3) as a value: it is fed the coverage of
/// every located map at each fix and keeps the dwell clocks, the cool-down and the exclusions
/// itself, so the rules are tested without a screen or a GPS.
///
/// Switch only when all of: the setting is on; the screen follows you; no tool is active and
/// nothing is presented; the fix is at most 10 s old with accuracy ≤ 50 m; no switch in the last
/// 60 s; the map was not opened in the last 30 s; and either you have been off the current sheet
/// by ≥ max(30 m, accuracy) for 5 s and another map covers you (by that much), or the best map —
/// the finest covering you by ≥ max(100 m, 2 × accuracy), the current one among those within
/// 10 % — is another map, ≥ 1.5× finer than the current one, and has covered you so for 20 s.
struct AutoSwitchPolicy: Sendable {
    /// A covering map must reach this far inside its sheet (or twice the accuracy) to be the best.
    static let bestMarginM = 100.0
    /// Maps within this fraction of the finest count as equally detailed (the current one stays).
    static let tieTolerance = 0.10
    /// Off the current sheet by at least this (or the accuracy)…
    static let offSheetM = 30.0
    /// …for this long.
    static let offSheetDwell: TimeInterval = 5
    /// "More detailed": this many times finer…
    static let finerRatio = 1.5
    /// …covering you for this long.
    static let finerDwell: TimeInterval = 20
    /// At most one automatic switch per minute.
    static let cooldown: TimeInterval = 60
    /// A map opened from the library is never switched away from in its first 30 s.
    static let openedGrace: TimeInterval = 30
    /// "Undo" (or picking a map yourself) holds for at most 15 min, or until you leave that sheet.
    static let holdDuration: TimeInterval = 15 * 60
    /// The fix must be this fresh…
    static let maxFixAge: TimeInterval = 10
    /// …and this accurate.
    static let maxAccuracyM = 50.0

    /// The GPS fix the rules look at.
    struct Fix: Equatable, Sendable {
        var accuracyM: Double
        var timestamp: Date

        init(accuracyM: Double, timestamp: Date) {
            self.accuracyM = accuracyM
            self.timestamp = timestamp
        }

        init(_ location: CLLocation) {
            accuracyM = location.horizontalAccuracy
            timestamp = location.timestamp
        }

        func isUsable(at now: Date) -> Bool {
            accuracyM >= 0 && accuracyM <= AutoSwitchPolicy.maxAccuracyM
                && now.timeIntervalSince(timestamp) <= AutoSwitchPolicy.maxFixAge
        }
    }

    /// The map the clocks below are about.
    private(set) var currentID: UUID?
    private(set) var openedAt: Date?
    private(set) var lastSwitchAt: Date?
    /// Maps auto-switching leaves alone (after "Undo"), and until when.
    private(set) var excluded: [UUID: Date] = [:]
    /// The current map was picked by hand: no "more detailed" switch away from it until this time
    /// or until you leave its sheet.
    private(set) var heldUntil: Date?
    /// Since when you have been off the current sheet.
    private var offSheetSince: Date?
    /// Since when each other map has covered you with the best-map margin.
    private var coveredSince: [UUID: Date] = [:]

    init() {}

    /// A screen shows `id`, opened from the library (or a link): the 30 s grace starts.
    mutating func opened(_ id: UUID, at now: Date) {
        reset(to: id)
        openedAt = now
    }

    /// Auto-switching showed `id`.
    mutating func switched(to id: UUID, at now: Date) {
        reset(to: id)
        lastSwitchAt = now
    }

    /// You picked `id` yourself ("Maps here"): it stays until you leave it (or 15 min).
    mutating func chose(_ id: UUID, at now: Date) {
        reset(to: id)
        heldUntil = now.addingTimeInterval(Self.holdDuration)
    }

    /// "Undo" after an automatic switch to `undone`: back on `previous`, and `undone` is left
    /// alone until you leave its sheet (or 15 min).
    mutating func undid(_ undone: UUID, backTo previous: UUID, at now: Date) {
        reset(to: previous)
        excluded[undone] = now.addingTimeInterval(Self.holdDuration)
        lastSwitchAt = now
    }

    /// A map is gone (deleted).
    mutating func forget(_ id: UUID) {
        excluded[id] = nil
        coveredSince[id] = nil
        if currentID == id { currentID = nil }
    }

    private mutating func reset(to id: UUID) {
        currentID = id
        openedAt = nil
        heldUntil = nil
        offSheetSince = nil
        coveredSince = [:]
    }

    /// The best map among `list`: the finest; among those within 10 % of it the current one, then
    /// the most recently opened (`recency`), then the finer.
    static func best(_ list: [MapCoverage], currentID: UUID?, recency: [UUID: Date]) -> MapCoverage? {
        let valid = list.filter { $0.metersPerPixel.isFinite && $0.metersPerPixel > 0 }
        guard let finest = valid.map(\.metersPerPixel).min() else { return nil }
        let near = valid.filter { $0.metersPerPixel <= finest * (1 + tieTolerance) }
        if let currentID, let current = near.first(where: { $0.mapID == currentID }) { return current }
        return near.max { a, b in
            let ra = recency[a.mapID] ?? .distantPast
            let rb = recency[b.mapID] ?? .distantPast
            if ra != rb { return ra < rb }
            if a.metersPerPixel != b.metersPerPixel { return a.metersPerPixel > b.metersPerPixel }
            return a.mapID.uuidString > b.mapID.uuidString
        }
    }

    /// Feeds one fix. `coverages`: every located map at your position (outside ones with a
    /// negative margin); `idle`: no tool active and nothing presented. Returns the map to switch
    /// to now, if any (then call `switched(to:at:)` once it is shown).
    mutating func evaluate(currentID: UUID, coverages: [MapCoverage], fix: Fix?, enabled: Bool, following: Bool,
                           idle: Bool, recency: [UUID: Date], now: Date) -> MapSwitchDecision? {
        // Shown by someone else (a new screen): start over, with the grace of a fresh opening.
        if self.currentID != currentID { opened(currentID, at: now) }
        let byID = Dictionary(coverages.map { ($0.mapID, $0) }, uniquingKeysWith: { a, _ in a })
        let shown = byID[currentID]

        // Exclusions and holds end with time…
        excluded = excluded.filter { _, until in now < until }
        if let held = heldUntil, now >= held { heldUntil = nil }

        guard let fix, fix.isUsable(at: now) else {
            // No evidence either way: the dwell clocks start again with the next good fix.
            offSheetSince = nil
            coveredSince = [:]
            return nil
        }
        // …or once a good fix has you off that sheet.
        excluded = excluded.filter { id, _ in (byID[id]?.insideMarginM ?? -1) >= 0 }
        if heldUntil != nil, (shown?.insideMarginM ?? -1) < 0 { heldUntil = nil }
        let accuracy = max(0, fix.accuracyM)
        let offBy = max(Self.offSheetM, accuracy)
        let margin = max(Self.bestMarginM, 2 * accuracy)

        // Dwell clocks run whatever the other conditions: they measure where you have been.
        if (shown?.insideMarginM ?? -.infinity) <= -offBy {
            if offSheetSince == nil { offSheetSince = now }
        } else {
            offSheetSince = nil
        }
        var covered: [UUID: Date] = [:]
        for c in coverages where c.mapID != currentID && c.insideMarginM >= margin {
            covered[c.mapID] = coveredSince[c.mapID] ?? now
        }
        coveredSince = covered

        guard enabled, following, idle else { return nil }
        if let last = lastSwitchAt, now.timeIntervalSince(last) < Self.cooldown { return nil }
        if let openedTime = openedAt, now.timeIntervalSince(openedTime) < Self.openedGrace { return nil }

        // Walked off: the best of the maps you are inside now.
        if let since = offSheetSince, now.timeIntervalSince(since) >= Self.offSheetDwell {
            let inside = coverages.filter { $0.mapID != currentID && excluded[$0.mapID] == nil && $0.insideMarginM >= offBy }
            if let pick = Self.best(inside, currentID: nil, recency: recency) {
                return MapSwitchDecision(mapID: pick.mapID, reason: .walkedOff)
            }
            return nil
        }

        // More detailed: the best map is another one, clearly finer, and has covered you a while.
        guard heldUntil == nil, let current = shown, current.insideMarginM > -offBy else { return nil }
        let candidates = coverages.filter {
            $0.insideMarginM >= margin && ($0.mapID == currentID || excluded[$0.mapID] == nil)
        }
        guard let bestMap = Self.best(candidates, currentID: currentID, recency: recency), bestMap.mapID != currentID,
              bestMap.metersPerPixel * Self.finerRatio <= current.metersPerPixel,
              let since = coveredSince[bestMap.mapID], now.timeIntervalSince(since) >= Self.finerDwell
        else { return nil }
        return MapSwitchDecision(mapID: bestMap.mapID, reason: .moreDetailed)
    }
}

/// Which of your maps cover where you are, and when to switch to a more detailed one as you walk
/// (docs/design/explorer.md, Map auto-switching). Use from the main thread.
@MainActor
@Observable
final class MapSwitcher {
    /// Maps covering your position, most detailed first (updated as you move).
    private(set) var mapsHere: [MapCoverage] = []
    /// Set before showing the world view from the walked-off banner: it opens centred on you and
    /// keeps following you (read once by `WorldView`).
    var worldViewFollowsUser = false

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored private var policy = AutoSwitchPolicy()
    @ObservationIgnored private var loop: ObservationLoop?
    /// `mapsHere` as last published, read without observing it (the loop must not wake itself).
    @ObservationIgnored private var published: [MapCoverage] = []

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
        // Reads the fix and the library: runs again whenever either changes.
        loop = ObservationLoop { [weak self] in
            self?.refreshMapsHere()
        }
    }

    private func refreshMapsHere() {
        guard let model else { return }
        let maps = model.library.maps
        var here: [MapCoverage] = []
        if let fix = model.location.location {
            here = Self.covering(LatLon(fix.coordinate), maps: maps)
        }
        guard here != published else { return }
        published = here
        mapsHere = here
    }

    // MARK: - Coverage

    /// How `map` covers `position` (nil without a georeference or when the position does not
    /// project): through the georeference and its warp, the sheet's rectangle.
    nonisolated static func coverage(of map: MapDocument, at position: LatLon) -> MapCoverage? {
        guard let g = map.georef, map.pixelWidth > 0, map.pixelHeight > 0,
              position.lat.isFinite, position.lon.isFinite else { return nil }
        let w = Double(map.pixelWidth)
        let h = Double(map.pixelHeight)
        let px = Georeference.Projector(g).pixel(for: position)
        guard px.x.isFinite, px.y.isFinite else { return nil }
        // A photo's homography also maps places behind the camera's horizon into the image: only
        // the side of the projective plane the sheet is on counts.
        let m = g.pixelToMercator.m
        let wHere = m[6] * Double(px.x) + m[7] * Double(px.y) + m[8]
        let wCentre = m[6] * w / 2 + m[7] * h / 2 + m[8]
        guard wHere * wCentre > 0 else { return nil }
        let x = Double(px.x)
        let y = Double(px.y)
        let edge = CGPoint(x: min(max(x, 0), w), y: min(max(y, 0), h))
        // The scale where you are, or at the nearest point of the sheet when you are beyond it.
        let mpp = g.metersPerPixel(atPixel: edge)
        guard mpp.isFinite, mpp > 0 else { return nil }
        let margin: Double
        if x >= 0, y >= 0, x <= w, y <= h {
            margin = min(min(x, w - x), min(y, h - y)) * mpp
        } else {
            margin = -Geodesy.distance(position, g.latLon(forPixel: edge))
        }
        guard margin.isFinite else { return nil }
        return MapCoverage(mapID: map.id, metersPerPixel: mpp, insideMarginM: margin)
    }

    /// The coverage of every located map at `position` (those it falls outside of included, with
    /// a negative margin).
    nonisolated static func coverages(_ maps: [MapDocument], at position: LatLon) -> [MapCoverage] {
        maps.compactMap { coverage(of: $0, at: position) }
    }

    /// The maps whose sheet holds `position`, most detailed first (then the most recently opened,
    /// as the library lists them).
    nonisolated static func covering(_ position: LatLon, maps: [MapDocument]) -> [MapCoverage] {
        let order = Dictionary(maps.enumerated().map { ($0.element.id, $0.offset) }, uniquingKeysWith: { a, _ in a })
        return coverages(maps, at: position)
            .filter { $0.insideMarginM >= 0 }
            .sorted { a, b in
                if a.metersPerPixel != b.metersPerPixel { return a.metersPerPixel < b.metersPerPixel }
                return (order[a.mapID] ?? 0) < (order[b.mapID] ?? 0)
            }
    }

    // MARK: - Auto-switching

    /// The map the screen showing `currentID` should switch to now, or nil to stay (the rules:
    /// covers you with a margin, ≥ 1.5× more detailed, for 20 s; only while following; hysteresis).
    func suggestion(currentID: UUID, fix: CLLocation, tracking: CanvasTrackingMode, now: Date) -> UUID? {
        evaluate(currentID: currentID, fix: fix, tracking: tracking, idle: true, now: now)?.mapID
    }

    /// `suggestion` with the reason, and whether the screen is idle (no tool, nothing presented).
    func evaluate(currentID: UUID, fix: CLLocation?, tracking: CanvasTrackingMode, idle: Bool,
                  now: Date) -> MapSwitchDecision? {
        guard let model else { return nil }
        let maps = model.library.maps.filter(\.isGeoreferenced)
        let coverages = fix.map { Self.coverages(maps, at: LatLon($0.coordinate)) } ?? []
        var recency: [UUID: Date] = [:]
        for map in maps { recency[map.id] = map.lastOpenedAt ?? map.createdAt }
        return policy.evaluate(currentID: currentID, coverages: coverages, fix: fix.map { AutoSwitchPolicy.Fix($0) },
                               enabled: model.settings.autoSwitchMaps, following: tracking.follows, idle: idle,
                               recency: recency, now: now)
    }

    /// A map screen appeared showing `id` (from the library, a link, the world view).
    func screenDidShow(_ id: UUID, now: Date = Date()) {
        policy.opened(id, at: now)
    }

    /// Auto-switching showed `id`.
    func didAutoSwitch(to id: UUID, now: Date = Date()) {
        policy.switched(to: id, at: now)
    }

    /// You picked `id` in "Maps here".
    func userChose(_ id: UUID, now: Date = Date()) {
        policy.chose(id, at: now)
    }

    /// "Undo" on the switch toast.
    func undo(_ undone: UUID, backTo previous: UUID, now: Date = Date()) {
        policy.undid(undone, backTo: previous, at: now)
    }

    /// A map was located again: nothing is cached per georeference, the next fix sees it.
    func mapDidLocate(_ id: UUID) {
        refreshMapsHere()
    }

    func forget(_ id: UUID) {
        policy.forget(id)
        published.removeAll { $0.mapID == id }
        mapsHere = published
    }
}

/// How far a map is continued beyond its sheet's edge with terrain (contours, the 3D view's
/// surroundings): the core's rule (`terrain::continuation_margin_m`: half the sheet's long side,
/// 3…15 km), in the map's pixels, kept where its projection holds.
enum MapContinuation {
    /// `terrain::CONTINUATION_MARGIN_M`.
    static let marginRangeM: ClosedRange<Double> = 3_000...15_000
    /// A photo's perspective may change the projective denominator by at most this much between
    /// the sheet's centre and the enlarged corners (beyond that the plane "bends away").
    static let maxPerspectiveChange = 0.25

    /// `terrain::continuation_margin_m`: half the long side, at least 3 km, at most 15 km.
    static func marginMeters(longSideM: Double) -> Double {
        guard longSideM.isFinite else { return marginRangeM.lowerBound }
        return min(max(longSideM / 2, marginRangeM.lowerBound), marginRangeM.upperBound)
    }

    /// The sheet's long side on the ground: the mean of its two longer opposite edges.
    static func longSideMeters(of map: MapDocument) -> Double? {
        guard let g = map.georef, map.pixelWidth > 0, map.pixelHeight > 0 else { return nil }
        let w = CGFloat(map.pixelWidth)
        let h = CGFloat(map.pixelHeight)
        let tl = g.latLon(forPixel: .zero)
        let tr = g.latLon(forPixel: CGPoint(x: w, y: 0))
        let br = g.latLon(forPixel: CGPoint(x: w, y: h))
        let bl = g.latLon(forPixel: CGPoint(x: 0, y: h))
        let across = (Geodesy.distance(tl, tr) + Geodesy.distance(bl, br)) / 2
        let down = (Geodesy.distance(tl, bl) + Geodesy.distance(tr, br)) / 2
        let long = max(across, down)
        return long.isFinite && long > 0 ? long : nil
    }

    /// Map pixels added on every side; 0 = no continuation (not located, the setting off).
    static func marginPixels(for map: MapDocument, enabled: Bool) -> CGFloat {
        guard enabled, let g = map.georef, let long = longSideMeters(of: map) else { return 0 }
        let w = Double(map.pixelWidth)
        let h = Double(map.pixelHeight)
        let mpp = g.metersPerPixel(atPixel: CGPoint(x: w / 2, y: h / 2))
        guard mpp.isFinite, mpp > 0 else { return 0 }
        var pixels = marginMeters(longSideM: long) / mpp
        // The denominator w(x, y) = m6·x + m7·y + m8 is linear, so it changes most at the enlarged
        // corners: |m6|·(W/2 + M) + |m7|·(H/2 + M) ≤ 25 % of |w(centre)| bounds M directly.
        let m = g.pixelToMercator.m
        let a = abs(m[6])
        let b = abs(m[7])
        if a + b > 0 {
            let wCentre = abs(m[6] * w / 2 + m[7] * h / 2 + m[8])
            let limit = (maxPerspectiveChange * wCentre - a * w / 2 - b * h / 2) / (a + b)
            pixels = min(pixels, max(0, limit))
        }
        guard pixels.isFinite, pixels >= 1 else { return 0 }
        return CGFloat(pixels.rounded(.down))
    }

    /// The sheet and its continuation, in map pixels.
    static func drawableRect(for map: MapDocument, enabled: Bool) -> CGRect {
        let m = marginPixels(for: map, enabled: enabled)
        return CGRect(origin: .zero, size: map.pixelSize).insetBy(dx: -m, dy: -m)
    }
}

/// Where you are relative to the map shown (the walked-off banner and the sheet's edge agree on
/// it): on the sheet, off it in the terrain continuing it, or beyond that.
enum MapWhereabouts: Equatable {
    case onSheet
    /// Off the sheet by `meters`, still inside the continuation (terrain shows around you).
    case offSheet(meters: Double)
    /// Beyond the continuation: `meters` from the sheet.
    case beyond(meters: Double)

    /// Off the sheet once you are this far beyond its edge (or half the accuracy, at most 25 m):
    /// a fix wobbling on the edge does not flash the banner.
    static func threshold(accuracyM: Double) -> Double {
        max(15, min(max(0, accuracyM) / 2, 25))
    }

    /// Nil when the map is not located or there is no fix.
    static func of(_ map: MapDocument, fix: CLLocation?, continuation: CGFloat) -> MapWhereabouts? {
        guard let fix, let g = map.georef else { return nil }
        let here = LatLon(fix.coordinate)
        guard let coverage = MapSwitcher.coverage(of: map, at: here) else {
            // Does not project (the far side of a photo's horizon): far away.
            let centre = map.center ?? here
            return .beyond(meters: Geodesy.distance(here, centre))
        }
        let off = -coverage.insideMarginM
        if off < threshold(accuracyM: fix.horizontalAccuracy) { return .onSheet }
        let px = g.pixel(for: here)
        let drawable = CGRect(origin: .zero, size: map.pixelSize).insetBy(dx: -continuation, dy: -continuation)
        if continuation > 0, drawable.contains(px) { return .offSheet(meters: off) }
        return .beyond(meters: off)
    }

    var isOnSheet: Bool { self == .onSheet }
}

/// How a map's scale reads in a list: "1:50 000" when its print resolution is known (a PDF), else
/// "4 m/px".
enum MapScaleText {
    static func text(metersPerPixel mpp: Double, dpi: Double?) -> String {
        guard mpp.isFinite, mpp > 0 else { return "" }
        if let dpi, dpi.isFinite, dpi > 0 {
            let denominator = mpp * dpi / 0.0254
            return "1:" + groupedThousands(roundedScale(denominator))
        }
        if mpp < 10 {
            let rounded = (mpp * 10).rounded() / 10
            return rounded == rounded.rounded() ? "\(Int(rounded)) m/px" : String(format: "%.1f m/px", rounded)
        }
        return "\(Int(mpp.rounded())) m/px"
    }

    /// Two significant figures (49 870 → 50 000, 24 100 → 24 000).
    static func roundedScale(_ value: Double) -> Int {
        guard value.isFinite, value >= 1 else { return 1 }
        let magnitude = pow(10, floor(log10(value)) - 1)
        return Int((value / magnitude).rounded() * magnitude)
    }

    /// "50 000" with a thin space, as printed on map sheets.
    static func groupedThousands(_ value: Int) -> String {
        var digits = String(value)
        var groups: [String] = []
        while digits.count > 3 {
            groups.insert(String(digits.suffix(3)), at: 0)
            digits.removeLast(3)
        }
        groups.insert(digits, at: 0)
        return groups.joined(separator: "\u{2009}")
    }
}
