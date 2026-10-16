import Foundation

/// Route statistics and elevation profiles from the terrain (`mm_terrain_profile` through
/// `TerrainProvider`): distance, ascent, descent, walking time (docs/design/explorer.md, Routes).
enum RoutePlanner {
    /// Stats and profile of `points` (a route, an imported track, the recorded trail). Downloads
    /// the terrain it lacks when online; offline, answers from what is on the device (distance
    /// alone when there is no terrain). Nil without a terrain store. Run off the main thread.
    static func profile(of points: [LatLon], terrain: TerrainProvider, maxZoom: Int = 13) async -> ElevationProfile? {
        // Filled in by the routes feature.
        nil
    }

    /// `RouteStats` for a route from its profile.
    static func stats(for route: Route, profile: ElevationProfile?) -> RouteStats {
        RouteStats(distanceM: profile?.distanceM ?? 0, ascentM: profile?.ascentM, descentM: profile?.descentM,
                   minElevationM: profile?.minM, maxElevationM: profile?.maxM, demZoom: profile?.zoom,
                   geometryHash: route.geometryHash)
    }
}
