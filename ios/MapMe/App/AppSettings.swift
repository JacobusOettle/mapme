import Foundation
import Observation
import UIKit

enum MapOrientationMode: String, Codable, CaseIterable, Identifiable {
    /// North (of the map) is up; the map never rotates by itself.
    case northUp
    /// The map rotates so your heading points up.
    case headingUp
    /// Two-finger rotation is allowed; no automatic rotation.
    /// Kept so old settings decode; it now behaves like `northUp` (rotation is its own setting).
    case free

    var id: String { rawValue }

    /// The choices offered in Settings (`free` is folded into "Allow map rotation").
    static let selectable: [MapOrientationMode] = [.northUp, .headingUp]

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

/// Colour of your position puck and heading beam on the map.
enum LocationColor: String, Codable, CaseIterable, Identifiable {
    case red
    case blue

    var id: String { rawValue }

    var label: String {
        switch self {
        case .red: return "Red"
        case .blue: return "Blue"
        }
    }

    /// Vivid system colours, resolved for the app's dark interface.
    var uiColor: UIColor {
        switch self {
        case .red: return UIColor(red: 1.0, green: 0.23, blue: 0.19, alpha: 1)
        case .blue: return UIColor(red: 0.04, green: 0.52, blue: 1.0, alpha: 1)
        }
    }
}

/// User preferences, persisted in UserDefaults.
@Observable
final class AppSettings {
    private let defaults: UserDefaults

    var units: DistanceUnits = .metric { didSet { defaults.set(units.rawValue, forKey: "units") } }
    var coordinateFormat: CoordinateFormat = .decimal { didSet { defaults.set(coordinateFormat.rawValue, forKey: "coordinateFormat") } }
    /// How a map opens: following you north up, or following with your heading (map turns as you turn).
    var orientationMode: MapOrientationMode = .northUp { didSet { defaults.set(orientationMode.rawValue, forKey: "orientationMode") } }
    /// Show the map with a dark (night) filter.
    var nightMap: Bool = false { didSet { defaults.set(nightMap, forKey: "nightMap") } }
    var keepScreenOn: Bool = true { didSet { defaults.set(keepScreenOn, forKey: "keepScreenOn") } }
    var showTrack: Bool = true { didSet { defaults.set(showTrack, forKey: "showTrack") } }
    var showPOIs: Bool = true { didSet { defaults.set(showPOIs, forKey: "showPOIs") } }
    /// Also draw the names read off the map as markers (they are already printed on the paper).
    var showMapLabels: Bool = false { didSet { defaults.set(showMapLabels, forKey: "showMapLabels") } }
    /// Colour of the position puck.
    var locationColor: LocationColor = .red { didSet { defaults.set(locationColor.rawValue, forKey: "locationColor") } }
    /// Two-finger rotation of the map.
    var allowMapRotation: Bool = true { didSet { defaults.set(allowMapRotation, forKey: "allowMapRotation") } }
    /// Fetch extra POIs from OpenStreetMap when online.
    var useOpenStreetMap: Bool = true { didSet { defaults.set(useOpenStreetMap, forKey: "useOpenStreetMap") } }
    /// How far from you a map may be when locating it from contours.
    var searchRadiusKm: Double = 20 { didSet { defaults.set(searchRadiusKm, forKey: "searchRadiusKm") } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let v = defaults.string(forKey: "units").flatMap(DistanceUnits.init(rawValue:)) { units = v }
        if let v = defaults.string(forKey: "coordinateFormat").flatMap(CoordinateFormat.init(rawValue:)) { coordinateFormat = v }
        if let v = defaults.string(forKey: "orientationMode").flatMap(MapOrientationMode.init(rawValue:)) {
            // "Free rotate" is now the separate "Allow map rotation" switch (on by default).
            orientationMode = v == .free ? .northUp : v
        }
        if let v = defaults.object(forKey: "nightMap") as? Bool { nightMap = v }
        if let v = defaults.object(forKey: "keepScreenOn") as? Bool { keepScreenOn = v }
        if let v = defaults.object(forKey: "showTrack") as? Bool { showTrack = v }
        if let v = defaults.object(forKey: "showPOIs") as? Bool { showPOIs = v }
        if let v = defaults.object(forKey: "showMapLabels") as? Bool { showMapLabels = v }
        if let v = defaults.string(forKey: "locationColor").flatMap(LocationColor.init(rawValue:)) { locationColor = v }
        if let v = defaults.object(forKey: "allowMapRotation") as? Bool { allowMapRotation = v }
        if let v = defaults.object(forKey: "useOpenStreetMap") as? Bool { useOpenStreetMap = v }
        if let v = defaults.object(forKey: "searchRadiusKm") as? Double { searchRadiusKm = v }
    }
}
