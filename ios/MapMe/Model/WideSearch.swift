import Foundation

/// Why locating a map automatically did not work, which decides what the result screen offers.
enum GeorefFailure: String, Codable, Equatable, Sendable {
    /// The contour search ran around you (or the places named on the map) and found no match:
    /// a wider search may find it.
    case noTerrainMatch
    /// Downloads stopped at the safety cap before the search was through.
    case stoppedAtTileLimit
    /// No grid could be read and there is no GPS fix (nor any hint) to search around.
    case noPosition
    /// No contour lines were found on the map.
    case noContours
    /// Terrain data could not be downloaded (offline).
    case terrainUnavailable
    /// The engine could not analyse the map.
    case engineError
    case cancelled
    /// Anything else (and values written by a newer version).
    case other

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = GeorefFailure(rawValue: raw) ?? .other
    }

    /// A search farther out could still find the map.
    var offersWiderSearch: Bool {
        self == .noTerrainMatch || self == .stoppedAtTileLimit
    }

    /// "Search wider" may still help (the library menu): not when the map has no contour lines to
    /// match or the engine could not read it.
    var widerSearchMayHelp: Bool {
        self != .noContours && self != .engineError
    }
}

/// The last automatic locate that did not find a map (stored in `MapDocument`), for "Search wider":
/// how far it searched and the names on the map worth looking up.
struct LocateAttempt: Codable, Hashable {
    var failure: GeorefFailure
    /// Radius of that search in km.
    var searchedRadiusKm: Double?
    /// `hint_queries` of its last response (names on the map, best first).
    var hintQueries: [String] = []

    enum CodingKeys: String, CodingKey {
        case failure, searchedRadiusKm, hintQueries
    }

    init(failure: GeorefFailure, searchedRadiusKm: Double?, hintQueries: [String] = []) {
        self.failure = failure
        self.searchedRadiusKm = searchedRadiusKm
        self.hintQueries = hintQueries
    }

    /// Lenient, so a malformed attempt never hides a map from the library.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        failure = (try? c.decodeIfPresent(GeorefFailure.self, forKey: .failure)) ?? .other
        searchedRadiusKm = c.lenientDouble(.searchedRadiusKm)
        hintQueries = (try? c.decodeIfPresent([String].self, forKey: .hintQueries)) ?? []
    }
}

/// What the user picked in the "Search wider" sheet.
struct WideSearchChoice: Equatable {
    var radiusKm: Double
    /// Look up the names on the map with Apple Maps and search there first.
    var useMapNames: Bool
    /// `mm_search_estimate` for this radius, if the core gave one.
    var estimate: SearchEstimate?

    /// Hard cap on terrain tiles for this search: 1.5× the estimate, never below the default.
    var tileCap: Int {
        WideSearch.tileCap(for: estimate)
    }
}

/// A button on the import result screen when a map could not be located.
enum ImportResultAction: Equatable {
    case searchWider
    case calibrate
    case openAnyway
    case tryAgain
}

/// Limits, presets and wording of "Search wider".
enum WideSearch {
    /// The first search's default radius; beyond it the core searches wide (in chunks, coarser).
    static let nearRadiusKm = 50.0
    /// The first search's radius is at most this (Settings: 10…100 km).
    static let firstSearchMaxRadiusKm = 100.0
    static let maxRadiusKm = 2_000.0
    static let customRangeKm: ClosedRange<Double> = 10...2_000
    /// Safety cap on terrain tiles for a wide search without an estimate (≈ 150 MB at zoom 9).
    static let defaultTileCap = 2_500
    /// Safety cap for the first search (≤ 100 km): its coarse search needs at most ~150 zoom-11
    /// tiles and refining a few candidates ~36 zoom-13 tiles each.
    static let nearTileCap = 600
    static let kmPerMile = 1.609344
    /// Above this much terrain data on mobile data, Start asks first.
    static let cellularConfirmationBytes = 50_000_000.0
    /// From this much terrain data the estimate says "Wi-Fi recommended".
    static let wifiRecommendedBytes = 25_000_000.0

    /// The radius choices in km: 100, 250, 500, 1 000 km, or 60, 150, 300, 600 mi.
    static func presetsKm(units: DistanceUnits) -> [Double] {
        switch units {
        case .metric: return [100, 250, 500, 1_000]
        case .imperial: return [60.0, 150.0, 300.0, 600.0].map { $0 * kmPerMile }
        }
    }

    /// The preset selected at first: the smallest at least 2.5× the radius already searched (250 km
    /// after the 50 km first search), else nil (custom).
    static func defaultPresetKm(after previousKm: Double?, units: DistanceUnits) -> Double? {
        let minimum = max(previousKm ?? nearRadiusKm, 1) * 2.5
        return presetsKm(units: units).first { $0 >= minimum - 0.5 }
    }

    /// Hard cap on terrain tiles for a search with this estimate.
    static func tileCap(for estimate: SearchEstimate?) -> Int {
        guard let estimate, estimate.tiles > 0 else { return defaultTileCap }
        return max(defaultTileCap, Int((Double(estimate.tiles) * 1.5).rounded(.up)))
    }

    /// The custom field's range in the user's units: 10–2 000 km, or 7–1 242 mi.
    static func customRange(units: DistanceUnits) -> ClosedRange<Int> {
        switch units {
        case .metric:
            return Int(customRangeKm.lowerBound)...Int(customRangeKm.upperBound)
        case .imperial:
            return Int((customRangeKm.lowerBound / kmPerMile).rounded(.up))...Int((customRangeKm.upperBound / kmPerMile).rounded(.down))
        }
    }

    /// The custom distance typed in the user's units as km, or nil when it is out of range.
    static func customKm(_ value: Double?, units: DistanceUnits) -> Double? {
        guard let value, value.isFinite else { return nil }
        let range = customRange(units: units)
        guard value >= Double(range.lowerBound), value <= Double(range.upperBound) else { return nil }
        let km = units == .metric ? value : value * kmPerMile
        return min(max(km, customRangeKm.lowerBound), customRangeKm.upperBound)
    }

    /// Which buttons the result screen shows for a map that was not located, first = primary.
    /// After the first search (up to 100 km) a terrain miss leads with "Search wider…"; after a
    /// wide one with "Calibrate manually" (still offering a wider search below the 2 000 km limit).
    /// Without a position to search around, names on the map (`hasMapNames`) can still say where
    /// to look, so "Search wider…" is offered after "Calibrate manually".
    static func resultActions(failure: GeorefFailure?, radiusKm: Double?, canCalibrate: Bool,
                              hasMapNames: Bool = false) -> [ImportResultAction] {
        guard canCalibrate else { return [.openAnyway] }
        let radius = radiusKm ?? nearRadiusKm
        if let failure, failure.offersWiderSearch, radius < maxRadiusKm - 0.5 {
            if radius <= firstSearchMaxRadiusKm + 0.5 {
                return [.searchWider, .calibrate, .openAnyway]
            }
            return [.calibrate, .searchWider, .openAnyway]
        }
        if failure == .noPosition, hasMapNames {
            return [.calibrate, .searchWider, .openAnyway]
        }
        return [.calibrate, .openAnyway, .tryAgain]
    }
}

/// Distances, progress and estimates as the "Search wider" screens say them.
enum SearchText {
    /// A whole number with a no-break space between thousands ("1 000", "2 410").
    static func grouped(_ n: Int) -> String {
        let digits = String(n.magnitude)
        var out = ""
        for (i, ch) in digits.enumerated() {
            if i > 0, (digits.count - i) % 3 == 0 { out.append("\u{00A0}") }
            out.append(ch)
        }
        return n < 0 ? "-" + out : out
    }

    /// "50 km", "1 000 km", "60 mi", "621 mi".
    static func distance(km: Double, units: DistanceUnits) -> String {
        let value = units == .metric ? km : km / WideSearch.kmPerMile
        let unit = units == .metric ? "km" : "mi"
        guard value.isFinite else { return "– \(unit)" }
        return "\(grouped(Int(value.rounded()))) \(unit)"
    }

    /// "Searched 320 of 1 000 km" (to 10 km once the radius is 100 or more).
    static func searched(km searchedKm: Double, of radiusKm: Double, units: DistanceUnits) -> String {
        let factor = units == .metric ? 1 : 1 / WideSearch.kmPerMile
        let radius = (radiusKm * factor).rounded()
        var reached = min(max(searchedKm * factor, 0), radius)
        reached = radius >= 100 ? (reached / 10).rounded() * 10 : reached.rounded()
        let unit = units == .metric ? "km" : "mi"
        return "Searched \(grouped(Int(min(reached, radius)))) of \(grouped(Int(radius))) \(unit)"
    }

    /// "About 45 MB", "Less than 1 MB", "About 1.2 GB" (decimal megabytes, like Settings).
    static func size(bytes: Double) -> String {
        let mb = max(bytes, 0) / 1_000_000
        if mb < 1 { return "Less than 1 MB" }
        if mb < 10 { return "About \(Int(mb.rounded())) MB" }
        if mb < 100 { return "About \(Int((mb / 5).rounded()) * 5) MB" }
        if mb < 1_000 { return "About \(Int((mb / 10).rounded()) * 10) MB" }
        return String(format: "About %.1f GB", mb / 1_000)
    }

    /// "under a minute", "a minute or two", "a few minutes", "about 25 minutes", "about an hour",
    /// "about 3 hours".
    static func duration(seconds: Double) -> String {
        let s = max(seconds, 0)
        if s < 60 { return "under a minute" }
        if s < 150 { return "a minute or two" }
        if s < 600 { return "a few minutes" }
        if s < 3_300 { return "about \(Int((s / 300).rounded()) * 5) minutes" }
        if s < 5_400 { return "about an hour" }
        return "about \(Int((s / 3_600).rounded())) hours"
    }

    /// "About 45 MB of terrain data, a few minutes. Wi-Fi recommended."
    static func estimateLine(_ estimate: SearchEstimate) -> String {
        var line = "\(size(bytes: estimate.approxBytes)) of terrain data, \(duration(seconds: estimate.approxSeconds))."
        if estimate.approxBytes >= WideSearch.wifiRecommendedBytes { line += " Wi-Fi recommended." }
        return line
    }

    /// Short form for a radius row: "about 45 MB".
    static func rowDetail(_ estimate: SearchEstimate) -> String {
        let s = size(bytes: estimate.approxBytes)
        return s.prefix(1).lowercased() + s.dropFirst()
    }

    /// Start asks before downloading this much over mobile data.
    static func needsCellularConfirmation(bytes: Double?, isExpensive: Bool) -> Bool {
        guard isExpensive, let bytes else { return false }
        return bytes > WideSearch.cellularConfirmationBytes
    }

    /// The headline of a wide search's progress screen.
    static func progressHeadline(_ status: SearchStatus?, radiusKm: Double, units: DistanceUnits) -> String {
        switch status?.phase {
        case .hints?:
            return "Checking places named on the map"
        case .near?:
            return "Searching near you"
        default:
            if let reachedM = status?.searchedRadiusM, reachedM > 0 {
                return Self.searched(km: reachedM / 1000, of: radiusKm, units: units)
            }
            return "Searching up to \(Self.distance(km: radiusKm, units: units)) away"
        }
    }
}
