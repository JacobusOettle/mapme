import Foundation
import XCTest
@testable import MapMe

/// The shared pieces of the explorer features (docs/design/explorer.md): the geo item store, tile
/// math and bounds, the terrain JSON contract as the app reads it, settings and file types.
final class ExplorerScaffoldTests: XCTestCase {
    private func temporaryDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("geo-items-\(UUID().uuidString)",
                                                                                isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    // MARK: - Geo item store

    @MainActor
    func testGeoItemStoreRoundTrip() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let store = GeoItemStore(directory: root)
        XCTAssertTrue(store.waypoints.isEmpty && store.routes.isEmpty && store.notes.isEmpty)

        let hut = Waypoint(name: "Bannerman Hut", position: LatLon(lat: -29.105, lon: 29.452), elevationM: 2800,
                           symbol: .hut, origin: .gpx(fileName: "Bannerman.gpx"))
        let route = Route(name: "Bannerman Pass", kind: .planned, points: [
            RoutePoint(LatLon(lat: -29.08, lon: 29.43), isAnchor: true, name: "Car park"),
            RoutePoint(LatLon(lat: -29.09, lon: 29.44), isAnchor: false),
            RoutePoint(LatLon(lat: -29.105, lon: 29.452), isAnchor: true, name: "Bannerman Hut"),
        ])
        let note = MapNote(position: LatLon(lat: -29.1, lon: 29.45), text: "Water here\nbut boil it")
        let revision = store.revision
        store.save(hut)
        store.save(route)
        store.save(note)
        XCTAssertGreaterThan(store.revision, revision)
        store.flush()

        let reloaded = GeoItemStore(directory: root)
        XCTAssertEqual(reloaded.waypoints, [hut])
        XCTAssertEqual(reloaded.routes, [route])
        XCTAssertEqual(reloaded.notes, [note])
        XCTAssertEqual(reloaded.route(id: route.id)?.anchors.map(\.name), ["Car park", "Bannerman Hut"])
        XCTAssertEqual(reloaded.notes.first?.title, "Water here")

        let area = GeoBounds(south: -29.2, west: 29.4, north: -29.0, east: 29.5)
        XCTAssertEqual(reloaded.waypoints(in: area).map(\.id), [hut.id])
        XCTAssertEqual(reloaded.routes(touching: area).map(\.id), [route.id])
        XCTAssertTrue(reloaded.notes(in: GeoBounds(south: 10, west: 10, north: 11, east: 11)).isEmpty)

        reloaded.deleteWaypoint(hut.id)
        reloaded.deleteRoute(route.id)
        reloaded.deleteNote(note.id)
        reloaded.flush()
        let empty = GeoItemStore(directory: root)
        XCTAssertTrue(empty.waypoints.isEmpty && empty.routes.isEmpty && empty.notes.isEmpty)
    }

    @MainActor
    func testUnreadableFileIsKeptAside() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        try Data("{not json".utf8).write(to: root.appendingPathComponent("waypoints.json"))
        let store = GeoItemStore(directory: root)
        XCTAssertTrue(store.waypoints.isEmpty)
        let names = try FileManager.default.contentsOfDirectory(atPath: root.path)
        XCTAssertTrue(names.contains { $0.hasPrefix("waypoints.unreadable-") }, "\(names)")
    }

    @MainActor
    func testDeletingANoteRemovesItsPhoto() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let store = GeoItemStore(directory: root)
        let photo = NotePhoto(fileName: "\(UUID().uuidString).jpg", pixelWidth: 4, pixelHeight: 3, placement: .exif)
        try Data([0xFF, 0xD8, 0xFF]).write(to: store.photoURL(for: photo))
        try Data([0xFF, 0xD8, 0xFF]).write(to: store.thumbnailURL(for: photo))
        let note = MapNote(position: LatLon(lat: -25.84, lon: 27.4), text: "", photo: photo)
        store.save(note)
        XCTAssertEqual(store.note(id: note.id)?.title, "Photo")
        store.deleteNote(note.id)
        store.flush()
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.photoURL(for: photo).path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.thumbnailURL(for: photo).path))
    }

    func testRouteGeometryHashFollowsThePoints() {
        let a = Route(name: "A", kind: .planned, points: [RoutePoint(lat: -25.8, lon: 27.4, isAnchor: true),
                                                           RoutePoint(lat: -25.81, lon: 27.41, isAnchor: true)])
        var b = a
        b.name = "B"
        XCTAssertEqual(a.geometryHash, b.geometryHash, "renaming keeps the geometry")
        b.points[1].lon += 1e-6
        XCTAssertNotEqual(a.geometryHash, b.geometryHash)
        var withStats = a
        withStats.stats = RouteStats(distanceM: 1500, ascentM: 120, geometryHash: a.geometryHash)
        XCTAssertNotNil(withStats.currentStats)
        withStats.points.append(RoutePoint(lat: -25.82, lon: 27.42, isAnchor: true))
        XCTAssertNil(withStats.currentStats, "stats of other points are stale")
        XCTAssertEqual(a.displayColor, .violet)
        XCTAssertEqual(Route(name: "T", kind: .gpxTrack, points: []).displayColor, .teal)
    }

    // MARK: - Bounds and tiles

    func testGeoBounds() throws {
        let b = GeoBounds(south: -30.68, west: 27.01, north: -28.57, east: 29.46)
        XCTAssertTrue(b.contains(LatLon(lat: -29.3, lon: 28.0)))
        XCTAssertFalse(b.contains(LatLon(lat: -29.3, lon: 30.0)))
        XCTAssertTrue(b.intersects(GeoBounds(south: -29, west: 29, north: -28, east: 30)))
        XCTAssertFalse(b.intersects(GeoBounds(south: -29, west: 30, north: -28, east: 31)))

        let pacific = GeoBounds(south: -20, west: 170, north: -10, east: -170)
        XCTAssertTrue(pacific.crossesAntimeridian)
        XCTAssertEqual(pacific.longitudeSpan, 20, accuracy: 1e-9)
        XCTAssertTrue(pacific.contains(LatLon(lat: -15, lon: 179)))
        XCTAssertTrue(pacific.contains(LatLon(lat: -15, lon: -175)))
        XCTAssertFalse(pacific.contains(LatLon(lat: -15, lon: 0)))
        XCTAssertEqual(pacific.center.lon, 180, accuracy: 1e-9)
        XCTAssertTrue(pacific.intersects(GeoBounds(south: -16, west: -179, north: -14, east: -178)))

        let grown = b.expanded(byMeters: 10_000)
        XCTAssertEqual(grown.south, b.south - 10_000 / 111_320, accuracy: 1e-9)
        XCTAssertLessThan(grown.west, b.west - 0.09)
        let enclosing = try XCTUnwrap(GeoBounds.enclosing([LatLon(lat: 1, lon: 2), LatLon(lat: -1, lon: 5)]))
        XCTAssertEqual(enclosing, GeoBounds(south: -1, west: 2, north: 1, east: 5))
    }

    func testTileMathMatchesTheCore() {
        // core/src/dem.rs `tile_math`: (-25.84, 27.40) is in zoom-12 tile 2359/2352.
        let p = LatLon(lat: -25.84, lon: 27.40)
        let t = TileMath.tile(for: p, zoom: 12)
        XCTAssertEqual(t, TileID(z: 12, x: 2359, y: 2352))
        let rect = TileMath.bounds(of: t)
        XCTAssertTrue(rect.contains(WebMercator.project(p)))
        XCTAssertEqual(TileMath.metersPerPixel(zoom: 0), 156_543.033_928, accuracy: 1e-3)
        let around = MercatorRect(minX: rect.minX + 10, minY: rect.minY + 10, maxX: rect.maxX + 10, maxY: rect.maxY - 10)
        XCTAssertEqual(TileMath.tiles(covering: around, zoom: 12),
                       [TileID(z: 12, x: 2359, y: 2352), TileID(z: 12, x: 2360, y: 2352)])
        XCTAssertEqual(TileRange(z: 12, x0: 1, x1: 2, y0: 5, y1: 7).count, 6)
        XCTAssertEqual(TileRange(z: 12, x0: 1, x1: 2, y0: 5, y1: 7).tiles.count, 6)
        XCTAssertTrue(TileRange(z: 12, x0: 1, x1: 2, y0: 5, y1: 7).contains(TileID(z: 12, x: 2, y: 6)))
    }

    // MARK: - Terrain JSON

    func testTerrainAnswersDecode() throws {
        let profileJSON = """
        {"status":"need_dem","distance_m":1520.5,"ascent_m":120.0,"descent_m":12.5,"min_m":1400.0,"max_m":1510.0,
         "samples":[[0.0,1400.0],[760.2,null],[1520.5,1510.0]],"vertex_distance_m":[0.0,1520.5],
         "vertex_elevation_m":[1400.0,null],"zoom":12,"missing_fraction":0.33,
         "need_tiles":[{"z":13,"x":4718,"y":4703}]}
        """
        let answer: TerrainAnswer<ElevationProfile> = TerrainStore.decode(profileJSON)
        XCTAssertEqual(answer.status, .needDEM)
        XCTAssertEqual(answer.needTiles, [TileID(z: 13, x: 4718, y: 4703)])
        let profile = try XCTUnwrap(answer.value)
        XCTAssertEqual(profile.samples.count, 3)
        XCTAssertNil(profile.samples[1].elevationM)
        XCTAssertEqual(profile.samples[2], ProfileSample(distanceM: 1520.5, elevationM: 1510))
        XCTAssertEqual(profile.vertexElevationM, [1400, nil])
        XCTAssertEqual(profile.ascentM, 120)
        // The profile round-trips (routes cache it).
        let again = try JSONDecoder().decode(ElevationProfile.self, from: JSONEncoder().encode(profile))
        XCTAssertEqual(again, profile)

        let failed: TerrainAnswer<ElevationProfile> = TerrainStore.decode(#"{"status":"failed","message":"no"}"#)
        XCTAssertEqual(failed.status, .failed("no"))
        XCTAssertNil(failed.value)
        let garbage: TerrainAnswer<ElevationProfile> = TerrainStore.decode("not json")
        XCTAssertFalse(garbage.isComplete)

        let planJSON = """
        {"status":"ok","tiles":7,"bytes":250000,"bytes_high":700000,
         "zooms":[{"z":12,"tiles":6,"bytes":220000,"bytes_high":600000,"ranges":[[10,11,20,22]]},
                  {"z":11,"tiles":1,"bytes":30000,"bytes_high":100000,"ranges":[[5,5,10,10]]}],
         "dropped":[{"z":13,"tiles":2400,"reason":"trailhead cap"}]}
        """
        let plan: TerrainAnswer<TilePlan> = TerrainStore.decode(planJSON)
        let p = try XCTUnwrap(plan.value)
        XCTAssertEqual(p.tiles, 7)
        XCTAssertEqual(p.zooms.map(\.z), [12, 11])
        XCTAssertEqual(p.ranges.map(\.count).reduce(0, +), 7)
        XCTAssertEqual(p.ranges.first, TileRange(z: 12, x0: 10, x1: 11, y0: 20, y1: 22))
        XCTAssertEqual(p.dropped.first?.z, 13)
    }

    func testTerrainRequestsEncodeTheContract() throws {
        var request = ProfileRequest(points: [LatLon(lat: -25.8, lon: 27.4), LatLon(lat: -25.81, lon: 27.41)])
        request.spacingM = 10
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(object["points"] as? [[Double]], [[-25.8, 27.4], [-25.81, 27.41]])
        XCTAssertEqual(object["spacing_m"] as? Double, 10)
        XCTAssertEqual(object["max_zoom"] as? Int, 13)
        XCTAssertNil(object["smoothing_m"], "nil fields are left to the core's defaults")

        let render = ContourRenderRequest(space: .map, rect: [0, 0, 512, 512], pixelToMercator: Homography.identity.m,
                                          style: .dark, scaleMPerPt: 2.5, hillshade: 0.3, lineScale: 3)
        let r = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(render)) as? [String: Any])
        XCTAssertEqual(r["space"] as? String, "map")
        XCTAssertEqual(r["style"] as? String, "dark")
        XCTAssertEqual(r["scale_m_per_pt"] as? Double, 2.5)
        XCTAssertEqual((r["pixel_to_mercator"] as? [Double])?.count, 9)

        let grid = GridRequest(rect: MercatorRect(minX: 1, minY: 2, maxX: 3, maxY: 4))
        let g = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(grid)) as? [String: Any])
        XCTAssertEqual(g["rect"] as? [Double], [1, 2, 3, 4])

        let trailhead = TilePlanRequest.trailhead(corners: [LatLon(lat: 1, lon: 2), LatLon(lat: 1, lon: 3),
                                                            LatLon(lat: 0, lon: 3), LatLon(lat: 0, lon: 2)])
        let t = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(trailhead)) as? [String: Any])
        XCTAssertEqual((t["trailhead"] as? [String: Any])?["corners"] as? [[Double]], [[1, 2], [1, 3], [0, 3], [0, 2]])
    }

    func testContourIntervalRuleMatchesTheCore() {
        // core/src/terrain.rs `interval_rule`.
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 1), 5)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 2.5), 10)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 5), 20)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 6), 25)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 10), 50)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 25), 100)
        XCTAssertEqual(ContourInterval.automatic(metersPerPoint: 1e6), 1000)
    }

    // MARK: - Settings and files

    func testExplorerSettingsPersist() throws {
        let suite = "explorer-settings-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let settings = AppSettings(defaults: defaults, arguments: [])
        XCTAssertTrue(settings.autoSwitchMaps)
        XCTAssertEqual(settings.batterySaver, .auto)
        XCTAssertNil(settings.walkingPaceFactor)
        settings.autoSwitchMaps = false
        settings.batterySaver = .on
        settings.walkingPaceFactor = 1.2
        settings.terrainExaggeration = 2
        let again = AppSettings(defaults: defaults, arguments: [])
        XCTAssertFalse(again.autoSwitchMaps)
        XCTAssertEqual(again.batterySaver, .on)
        XCTAssertEqual(again.walkingPaceFactor, 1.2)
        XCTAssertEqual(again.terrainExaggeration, 2)
        again.walkingPaceFactor = nil
        XCTAssertNil(AppSettings(defaults: defaults, arguments: []).walkingPaceFactor)
    }

    func testIncomingFileKinds() {
        XCTAssertEqual(IncomingFile.kind(of: URL(fileURLWithPath: "/tmp/Bannerman Pass.GPX")), .gpx)
        XCTAssertEqual(IncomingFile.kind(of: URL(fileURLWithPath: "/tmp/map.pdf")), .map)
        XCTAssertEqual(IncomingFile.kind(of: URL(fileURLWithPath: "/tmp/photo.jpg")), .map)
    }

    func testOfflineDetailLevels() {
        XCTAssertEqual(OfflineDetail.standard.zooms, [8, 9, 10, 11, 12])
        XCTAssertEqual(OfflineDetail.high.zooms, [8, 9, 10, 11, 12, 13])
        XCTAssertEqual(TerrainOwner.map(UUID(uuidString: "00000000-0000-0000-0000-000000000001")!).fileStem,
                       "map-00000000-0000-0000-0000-000000000001")
    }
}
