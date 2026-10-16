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
        model.navigator.refreshLearnedPace()
        #if DEBUG
        let args = ProcessInfo.processInfo.arguments
        // Read the mocked clock now, so it starts where the test asked.
        _ = RoutesClock.offset
        if args.contains("-seedExplorerItems") || args.contains("-followSampleRoute") {
            RoutesSample.seed(into: model.geoItems)
        }
        if args.contains("-importSampleGPX") {
            RoutesSample.importSampleGPX(model)
        }
        if args.contains("-followSampleRoute"), let route = model.geoItems.route(id: RoutesSample.routeID) {
            model.navigator.follow(route)
        }
        #endif
    }
}

/// Waypoints used to be `MapPOI`s (kind `.waypoint`, source `.user`) inside one map's
/// `meta.json`. They are global geo items now.
@MainActor
enum GeoItemMigration {
    /// Moves every map's user waypoints into `store` (origin `.migrated(mapID:)`) and removes them
    /// from the maps. Idempotent: a map without user waypoints is left alone, and a waypoint keeps
    /// its id, so one already moved (the map's save failed last time) is not added twice.
    static func migrateMapWaypoints(library: MapLibrary, store: GeoItemStore) {
        var moved: [Waypoint] = []
        for map in library.maps {
            let old = map.pois.filter { $0.kind == .waypoint && $0.source == .user }
            guard !old.isEmpty else { continue }
            for poi in old where store.waypoint(id: poi.id) == nil && !moved.contains(where: { $0.id == poi.id }) {
                var w = Waypoint(id: poi.id, name: poi.name, position: poi.position)
                w.elevationM = poi.elevationM
                w.createdAt = map.createdAt
                w.origin = .migrated(mapID: map.id)
                moved.append(w)
            }
            var updated = map
            updated.pois.removeAll { $0.kind == .waypoint && $0.source == .user }
            // The store first: the waypoints are never only in memory while gone from the map.
            store.save(moved)
            store.flush()
            moved.removeAll()
            library.save(updated)
        }
    }
}

#if DEBUG
/// The routes' sample content for UI tests and screenshots, on the Grootkloof sample sheet
/// (Magaliesberg, about −25.81…−25.89, 27.37…27.43): a planned route through the test position
/// (−25.8405, 27.3920), three waypoints, and a GPX file with a route, a track and waypoints.
@MainActor
enum RoutesSample {
    static let routeID = UUID(uuidString: "6A3C1F0E-5B7D-4E2A-9C11-2F8D0B6E7A01") ?? UUID()
    static let waypointIDs = [
        UUID(uuidString: "6A3C1F0E-5B7D-4E2A-9C11-2F8D0B6E7A02") ?? UUID(),
        UUID(uuidString: "6A3C1F0E-5B7D-4E2A-9C11-2F8D0B6E7A03") ?? UUID(),
        UUID(uuidString: "6A3C1F0E-5B7D-4E2A-9C11-2F8D0B6E7A04") ?? UUID(),
    ]

    /// The planned route and the waypoints (once: fixed ids).
    static func seed(into store: GeoItemStore) {
        if store.route(id: routeID) == nil {
            let points = [
                RoutePoint(lat: -25.8460, lon: 27.3870, isAnchor: true, name: "Grootkloof Camp"),
                RoutePoint(lat: -25.8432, lon: 27.3893, isAnchor: true),
                RoutePoint(lat: -25.8405, lon: 27.3920, isAnchor: true),
                RoutePoint(lat: -25.8371, lon: 27.3958, isAnchor: true, name: "Waterfall"),
                RoutePoint(lat: -25.8340, lon: 27.3990, isAnchor: true),
                RoutePoint(lat: -25.8312, lon: 27.4041, isAnchor: true),
                RoutePoint(lat: -25.8290, lon: 27.4080, isAnchor: true, name: "Hamerkop Ridge"),
            ]
            store.save(Route(id: routeID, name: "Hamerkop Ridge", kind: .planned, points: points))
        }
        let waypoints = [
            Waypoint(id: waypointIDs[0], name: "Grootkloof Camp", position: LatLon(lat: -25.8460, lon: 27.3870),
                     symbol: .camp),
            Waypoint(id: waypointIDs[1], name: "Waterfall", position: LatLon(lat: -25.8366, lon: 27.3950),
                     symbol: .water, comment: "Pools below the fall; filter the water."),
            Waypoint(id: waypointIDs[2], name: "Hamerkop Ridge", position: LatLon(lat: -25.8290, lon: 27.4080),
                     symbol: .peak),
        ].filter { store.waypoint(id: $0.id) == nil }
        store.save(waypoints)
    }

    /// Imports `sampleGPX` through the real path (a file, `GPXService.importFile`).
    static func importSampleGPX(_ model: AppModel) {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("Grootkloof Traverse.gpx")
        do {
            try Data(sampleGPX.utf8).write(to: url, options: .atomic)
            model.gpx.importFile(at: url)
        } catch {
            print("RoutesSample: could not write the sample GPX: \(error)")
        }
    }

    /// Gaia GPS style: metadata, waypoints with `sym`, a route and a recorded track with times.
    static let sampleGPX = """
    <?xml version="1.0" encoding="UTF-8"?>
    <gpx xmlns="http://www.topografix.com/GPX/1/1" version="1.1" creator="GaiaGPS">
      <metadata><name>Grootkloof Traverse</name></metadata>
      <wpt lat="-25.8215" lon="27.3802"><ele>1480</ele><name>Kloof Pool</name><desc>Deep pool, cold</desc><sym>water</sym></wpt>
      <wpt lat="-25.8552" lon="27.4168"><ele>1610</ele><name>Beacon</name><sym>peak</sym></wpt>
      <wpt lat="-25.8740" lon="27.3920"><name>Gate</name><sym>parking</sym></wpt>
      <rte>
        <name>Kloof Descent</name>
        <rtept lat="-25.8215" lon="27.3802"><name>Kloof Pool</name></rtept>
        <rtept lat="-25.8260" lon="27.3850"></rtept>
        <rtept lat="-25.8330" lon="27.3880"></rtept>
        <rtept lat="-25.8405" lon="27.3920"><name>Junction</name></rtept>
      </rte>
      <trk>
        <name>Beacon Loop (recorded)</name>
        <trkseg>
          <trkpt lat="-25.8740" lon="27.3920"><ele>1390</ele><time>2026-05-02T07:00:00Z</time></trkpt>
          <trkpt lat="-25.8700" lon="27.3975"><ele>1425</ele><time>2026-05-02T07:09:00Z</time></trkpt>
          <trkpt lat="-25.8655" lon="27.4030"><ele>1470</ele><time>2026-05-02T07:19:00Z</time></trkpt>
          <trkpt lat="-25.8610" lon="27.4090"><ele>1530</ele><time>2026-05-02T07:31:00Z</time></trkpt>
          <trkpt lat="-25.8552" lon="27.4168"><ele>1610</ele><time>2026-05-02T07:46:00Z</time></trkpt>
        </trkseg>
        <trkseg>
          <trkpt lat="-25.8580" lon="27.4120"><ele>1560</ele><time>2026-05-02T08:05:00Z</time></trkpt>
          <trkpt lat="-25.8650" lon="27.4060"><ele>1480</ele><time>2026-05-02T08:17:00Z</time></trkpt>
          <trkpt lat="-25.8720" lon="27.3990"><ele>1410</ele><time>2026-05-02T08:29:00Z</time></trkpt>
          <trkpt lat="-25.8740" lon="27.3920"><ele>1390</ele><time>2026-05-02T08:38:00Z</time></trkpt>
        </trkseg>
      </trk>
    </gpx>
    """
}
#endif
