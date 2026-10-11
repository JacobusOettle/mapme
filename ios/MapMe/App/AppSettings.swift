import Foundation
import Observation

enum MapOrientationMode: String, Codable, CaseIterable, Identifiable {
    /// North (of the map) is up; the map never rotates by itself.
    case northUp
    /// The map rotates so your heading points up.
    case headingUp
    /// Two-finger rotation is allowed; no automatic rotation.
    case free

    var id: String { rawValue }

    var label: String {
        switch self {
        case .northUp: return "North up"
        case .headingUp: return "Heading up"
        case .free: return "Free rotate"
        }
    }

    var symbolName: String {
        switch self {
        case .northUp: return "location.north.line.fill"
        case .headingUp: return "location.north.circle.fill"
        case .free: return "arrow.triangle.2.circlepath"
        }
    }
}

/// User preferences, persisted in UserDefaults.
@Observable
final class AppSettings {
    private let defaults: UserDefaults

    var units: DistanceUnits = .metric { didSet { defaults.set(units.rawValue, forKey: "units") } }
    var coordinateFormat: CoordinateFormat = .decimal { didSet { defaults.set(coordinateFormat.rawValue, forKey: "coordinateFormat") } }
    var orientationMode: MapOrientationMode = .northUp { didSet { defaults.set(orientationMode.rawValue, forKey: "orientationMode") } }
    /// Show the map with a dark (night) filter.
    var nightMap: Bool = false { didSet { defaults.set(nightMap, forKey: "nightMap") } }
    var keepScreenOn: Bool = true { didSet { defaults.set(keepScreenOn, forKey: "keepScreenOn") } }
    var showTrack: Bool = true { didSet { defaults.set(showTrack, forKey: "showTrack") } }
    var showPOIs: Bool = true { didSet { defaults.set(showPOIs, forKey: "showPOIs") } }
    /// Fetch extra POIs from OpenStreetMap when online.
    var useOpenStreetMap: Bool = true { didSet { defaults.set(useOpenStreetMap, forKey: "useOpenStreetMap") } }
    /// How far from you a map may be when locating it from contours.
    var searchRadiusKm: Double = 20 { didSet { defaults.set(searchRadiusKm, forKey: "searchRadiusKm") } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let v = defaults.string(forKey: "units").flatMap(DistanceUnits.init(rawValue:)) { units = v }
        if let v = defaults.string(forKey: "coordinateFormat").flatMap(CoordinateFormat.init(rawValue:)) { coordinateFormat = v }
        if let v = defaults.string(forKey: "orientationMode").flatMap(MapOrientationMode.init(rawValue:)) { orientationMode = v }
        if let v = defaults.object(forKey: "nightMap") as? Bool { nightMap = v }
        if let v = defaults.object(forKey: "keepScreenOn") as? Bool { keepScreenOn = v }
        if let v = defaults.object(forKey: "showTrack") as? Bool { showTrack = v }
        if let v = defaults.object(forKey: "showPOIs") as? Bool { showPOIs = v }
        if let v = defaults.object(forKey: "useOpenStreetMap") as? Bool { useOpenStreetMap = v }
        if let v = defaults.object(forKey: "searchRadiusKm") as? Double { searchRadiusKm = v }
    }
}
