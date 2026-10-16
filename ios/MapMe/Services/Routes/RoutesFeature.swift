import Foundation

/// Routes and waypoints, elevation profiles and daylight (docs/design/explorer.md, features 1 and
/// 3): the routes feature's lifecycle hooks, called by `FeatureHooks`.
@MainActor
enum RoutesFeature {
    /// Wire `model.navigator` and `model.gpx` to the location service, the geo item store and the
    /// terrain. No I/O, no prompts.
    static func attach(_ model: AppModel) {
        model.navigator.attach(model)
        model.gpx.attach(model)
    }

    /// Moves the waypoints dropped on single maps into the global store (once), and handles the
    /// routes' Debug launch arguments (`-seedExplorerItems`, `-importSampleGPX`,
    /// `-followSampleRoute`, `-mockNow`).
    static func appDidLaunch(_ model: AppModel) async {
        GeoItemMigration.migrateMapWaypoints(library: model.library, store: model.geoItems)
    }
}

/// Waypoints used to be `MapPOI`s (kind `.waypoint`, source `.user`) inside one map's
/// `meta.json`. They are global geo items now.
@MainActor
enum GeoItemMigration {
    /// Moves every map's user waypoints into `store` (origin `.migrated(mapID:)`) and removes them
    /// from the maps. Idempotent: a map without user waypoints is left alone.
    static func migrateMapWaypoints(library: MapLibrary, store: GeoItemStore) {
        // Filled in by the routes feature.
    }
}
