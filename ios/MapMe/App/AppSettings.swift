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

/// How the map raster is drawn.
enum MapAppearance: String, Codable, CaseIterable, Identifiable {
    /// Redrawn dark by the core (`DarkStyle`): dark paper, light ink, every feature keeps its colour.
    case dark
    /// As printed.
    case paper

    var id: String { rawValue }

    var label: String {
        switch self {
        case .dark: return "Dark"
        case .paper: return "Paper"
        }
    }

    var symbolName: String {
        switch self {
        case .dark: return "moon.fill"
        case .paper: return "map"
        }
    }
}

/// Battery saver (docs/design/explorer.md, Battery saver).
enum BatterySaverMode: String, Codable, CaseIterable, Identifiable {
    case off
    /// On while the iPhone is in Low Power Mode.
    case auto
    case on

    var id: String { rawValue }

    var label: String {
        switch self {
        case .off: return "Off"
        case .auto: return "With Low Power Mode"
        case .on: return "Always"
        }
    }
}

/// User preferences, persisted in UserDefaults.
@Observable
final class AppSettings {
    private let defaults: UserDefaults
    /// Off during `init`, and for the whole run when a launch argument overrides the appearance
    /// (the override lasts for this run only and never replaces the stored choice).
    @ObservationIgnored private var persistsMapAppearance = false

    var units: DistanceUnits = .metric { didSet { defaults.set(units.rawValue, forKey: "units") } }
    var coordinateFormat: CoordinateFormat = .decimal { didSet { defaults.set(coordinateFormat.rawValue, forKey: "coordinateFormat") } }
    /// How a map opens: following you north up, or following with your heading (map turns as you turn).
    var orientationMode: MapOrientationMode = .northUp { didSet { defaults.set(orientationMode.rawValue, forKey: "orientationMode") } }
    /// Dark (the default) or as printed. The old "nightMap" switch is ignored: maps are dark now.
    var mapAppearance: MapAppearance = .dark {
        didSet { if persistsMapAppearance { defaults.set(mapAppearance.rawValue, forKey: "mapAppearance") } }
    }
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
    /// How far from you the first contour search looks for a map (a wider search is offered when it
    /// fails). 50 km by default, 10…100 km in Settings.
    var searchRadiusKm: Double = AppSettings.defaultSearchRadiusKm {
        didSet { defaults.set(searchRadiusKm, forKey: "searchRadiusKm") }
    }

    // Explorer features (docs/design/explorer.md).

    /// Switch to the most detailed map covering you as you walk (Settings → Maps).
    var autoSwitchMaps: Bool = true { didSet { defaults.set(autoSwitchMaps, forKey: "autoSwitchMaps") } }
    /// Continue a map beyond its sheet's edge with terrain contours.
    var showOffMapTerrain: Bool = true { didSet { defaults.set(showOffMapTerrain, forKey: "showOffMapTerrain") } }
    /// Show the names and places of your other maps on this one (beyond the sheet, too).
    var showOtherMapsPOIs: Bool = true { didSet { defaults.set(showOtherMapsPOIs, forKey: "showOtherMapsPOIs") } }
    var batterySaver: BatterySaverMode = .auto {
        didSet { defaults.set(batterySaver.rawValue, forKey: "batterySaver") }
    }
    /// Warn (haptic, banner, notification in the background) when you leave the route you follow.
    var offRouteAlerts: Bool = true { didSet { defaults.set(offRouteAlerts, forKey: "offRouteAlerts") } }
    /// Your walking pace against Tobler's hiking function (1 = as the model, 1.2 = 20 % slower);
    /// nil: learned from your recorded tracks.
    var walkingPaceFactor: Double? = nil {
        didSet {
            if let walkingPaceFactor { defaults.set(walkingPaceFactor, forKey: "walkingPaceFactor") } else {
                defaults.removeObject(forKey: "walkingPaceFactor")
            }
        }
    }
    /// Keep the terrain around every map you locate on this iPhone (trailhead caching).
    var cacheTerrainAroundMaps: Bool = true {
        didSet { defaults.set(cacheTerrainAroundMaps, forKey: "cacheTerrainAroundMaps") }
    }
    /// Download offline regions over mobile data without asking.
    var offlineDownloadsOnCellular: Bool = false {
        didSet { defaults.set(offlineDownloadsOnCellular, forKey: "offlineDownloadsOnCellular") }
    }
    /// Vertical exaggeration of the 3D view (1…3).
    var terrainExaggeration: Double = 1.5 {
        didSet { defaults.set(terrainExaggeration, forKey: "terrainExaggeration") }
    }

    static let defaultSearchRadiusKm = 50.0
    static let searchRadiusRangeKm: ClosedRange<Double> = 10...100
    /// The default before version 2 of the setting; a stored value equal to it was never chosen
    /// on purpose and becomes the new default.
    static let legacyDefaultSearchRadiusKm = 20.0
    static let searchRadiusVersion = 2

    /// - Parameter arguments: launch arguments; `-mapAppearance dark|paper` overrides the stored
    ///   appearance for this run without saving it (UI tests and screenshots).
    init(defaults: UserDefaults = .standard, arguments: [String] = ProcessInfo.processInfo.arguments) {
        self.defaults = defaults
        if let v = defaults.string(forKey: "units").flatMap(DistanceUnits.init(rawValue:)) { units = v }
        if let v = defaults.string(forKey: "coordinateFormat").flatMap(CoordinateFormat.init(rawValue:)) { coordinateFormat = v }
        if let v = defaults.string(forKey: "orientationMode").flatMap(MapOrientationMode.init(rawValue:)) {
            // "Free rotate" is now the separate "Allow map rotation" switch (on by default).
            orientationMode = v == .free ? .northUp : v
        }
        if let v = defaults.string(forKey: "mapAppearance").flatMap(MapAppearance.init(rawValue:)) { mapAppearance = v }
        if let v = Self.launchValue("-mapAppearance", in: arguments).flatMap(MapAppearance.init(rawValue:)) {
            mapAppearance = v
        } else {
            persistsMapAppearance = true
        }
        if let v = defaults.object(forKey: "keepScreenOn") as? Bool { keepScreenOn = v }
        if let v = defaults.object(forKey: "showTrack") as? Bool { showTrack = v }
        if let v = defaults.object(forKey: "showPOIs") as? Bool { showPOIs = v }
        if let v = defaults.object(forKey: "showMapLabels") as? Bool { showMapLabels = v }
        if let v = defaults.string(forKey: "locationColor").flatMap(LocationColor.init(rawValue:)) { locationColor = v }
        if let v = defaults.object(forKey: "allowMapRotation") as? Bool { allowMapRotation = v }
        if let v = defaults.object(forKey: "useOpenStreetMap") as? Bool { useOpenStreetMap = v }
        if let v = defaults.object(forKey: "searchRadiusKm") as? Double {
            let migrated = Self.migratedSearchRadius(v, storedVersion: defaults.integer(forKey: "searchRadiusVersion"))
            searchRadiusKm = migrated
            if migrated != v { defaults.set(migrated, forKey: "searchRadiusKm") }
        }
        defaults.set(Self.searchRadiusVersion, forKey: "searchRadiusVersion")
        if let v = defaults.object(forKey: "autoSwitchMaps") as? Bool { autoSwitchMaps = v }
        if let v = defaults.object(forKey: "showOffMapTerrain") as? Bool { showOffMapTerrain = v }
        if let v = defaults.object(forKey: "showOtherMapsPOIs") as? Bool { showOtherMapsPOIs = v }
        if let v = defaults.string(forKey: "batterySaver").flatMap(BatterySaverMode.init(rawValue:)) { batterySaver = v }
        if let v = defaults.object(forKey: "offRouteAlerts") as? Bool { offRouteAlerts = v }
        if let v = defaults.object(forKey: "walkingPaceFactor") as? Double, v.isFinite, v > 0 { walkingPaceFactor = v }
        if let v = defaults.object(forKey: "cacheTerrainAroundMaps") as? Bool { cacheTerrainAroundMaps = v }
        if let v = defaults.object(forKey: "offlineDownloadsOnCellular") as? Bool { offlineDownloadsOnCellular = v }
        if let v = defaults.object(forKey: "terrainExaggeration") as? Double, v.isFinite {
            terrainExaggeration = min(max(v, 1), 3)
        }
    }

    /// A stored search radius brought up to date: the old 20 km default (version < 2) becomes
    /// 50 km, and anything is kept within the slider's 10…100 km.
    static func migratedSearchRadius(_ stored: Double, storedVersion: Int) -> Double {
        guard stored.isFinite else { return defaultSearchRadiusKm }
        var km = stored
        if storedVersion < searchRadiusVersion, abs(km - legacyDefaultSearchRadiusKm) < 0.5 {
            km = defaultSearchRadiusKm
        }
        return min(max(km, searchRadiusRangeKm.lowerBound), searchRadiusRangeKm.upperBound)
    }

    /// The value after `flag` in launch arguments (`-flag value`), if any.
    static func launchValue(_ flag: String, in arguments: [String]) -> String? {
        guard let i = arguments.firstIndex(of: flag), i + 1 < arguments.count else { return nil }
        return arguments[i + 1]
    }
}
