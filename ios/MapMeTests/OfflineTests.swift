import CoreLocation
import XCTest
@testable import MapMe

/// Offline terrain (docs/design/explorer.md, Offline regions and Trailhead caching): plans turned
/// into tiles and estimates, the pinned store against the cache's eviction, the download engine
/// (retries, offline stops, resuming after an interruption) through a fake tile server, the
/// region service across a relaunch, and matching offline from pinned tiles (the need_dem loop
/// after a cutoff, and the real core on the Grootkloof sheet with the network off).
final class OfflineTests: XCTestCase {
    override func tearDown() {
        OfflineTestTileServer.reset()
        super.tearDown()
    }

    // MARK: - Plans → tiles

    func testOrderedTilesGoCoarseFirstRowByRowWithoutDuplicates() {
        let plan = Self.plan([TileRange(z: 12, x0: 10, x1: 11, y0: 20, y1: 21),
                              TileRange(z: 10, x0: 2, x1: 2, y0: 5, y1: 5),
                              TileRange(z: 12, x0: 11, x1: 11, y0: 21, y1: 21)])
        let tiles = OfflinePlanner.orderedTiles(plan)
        XCTAssertEqual(tiles, [TileID(z: 10, x: 2, y: 5),
                               TileID(z: 12, x: 10, y: 20), TileID(z: 12, x: 11, y: 20),
                               TileID(z: 12, x: 10, y: 21), TileID(z: 12, x: 11, y: 21)],
                       "zoom 10 first, then zoom 12 row by row; the repeated tile once")
    }

    func testSamplesSpreadOverTheTwoFinestZooms() {
        let plan = Self.plan([TileRange(z: 10, x0: 0, x1: 1, y0: 0, y1: 1),
                              TileRange(z: 11, x0: 0, x1: 9, y0: 0, y1: 9),
                              TileRange(z: 12, x0: 0, x1: 19, y0: 0, y1: 19)])
        let samples = OfflinePlanner.sampleTiles(plan)
        XCTAssertEqual(samples.count, 16)
        XCTAssertEqual(samples.filter { $0.z == 12 }.count, 8)
        XCTAssertEqual(samples.filter { $0.z == 11 }.count, 8)
        XCTAssertEqual(Set(samples).count, 16, "no tile measured twice")
        let rows = Set(samples.filter { $0.z == 12 }.map(\.y))
        XCTAssertGreaterThanOrEqual(rows.count, 7, "spread over the rows, not bunched at the top: \(rows.sorted())")
        XCTAssertLessThan(rows.min() ?? 99, 3)
        XCTAssertGreaterThan(rows.max() ?? 0, 16)
        // Fewer tiles than samples: each once.
        XCTAssertEqual(Set(OfflinePlanner.sampleTiles(Self.plan([TileRange(z: 9, x0: 0, x1: 1, y0: 0, y1: 0)]))).count, 2)
    }

    func testMeasuredEstimateScalesEachZoomByItsSamples() throws {
        // Zoom 10 from the table only; 11 and 12 measured (8 samples each).
        let plan = TilePlan(tiles: 525, bytes: 0, bytesHigh: 0, zooms: [
            TilePlan.Zoom(z: 10, tiles: 25, bytes: 25 * 61_000, bytesHigh: 25 * 94_000, ranges: []),
            TilePlan.Zoom(z: 11, tiles: 100, bytes: 100 * 53_000, bytesHigh: 100 * 127_000, ranges: []),
            TilePlan.Zoom(z: 12, tiles: 400, bytes: 400 * 44_000, bytesHigh: 400 * 117_000, ranges: []),
        ], dropped: [])
        var samples: [TileID: Int64] = [:]
        for i in 0..<8 {
            samples[TileID(z: 12, x: i, y: 0)] = 110_000 + Int64(i) * 2_000      // mean 117 000
            samples[TileID(z: 11, x: i, y: 0)] = 127_000
        }
        let bytes = try XCTUnwrap(OfflinePlanner.measuredBytes(plan, samples: samples))
        let measured = 400.0 * 117_000 + 100 * 127_000
        let ratio = measured / (400.0 * 44_000 + 100 * 53_000)
        // Zoom 10 scaled like the others but kept within 1.5× its high-resolution table size.
        let z10 = min(25.0 * 61_000 * ratio, 25.0 * 94_000 * 1.5)
        XCTAssertEqual(bytes, measured + z10, accuracy: 1)
        XCTAssertGreaterThan(bytes, plan.zooms.reduce(0) { $0 + $1.bytes } * 2, "high-resolution terrain weighs 2–3× the SRTM table")

        // Too few answers: no measured estimate.
        XCTAssertNil(OfflinePlanner.measuredBytes(plan, samples: [TileID(z: 12, x: 0, y: 0): 50_000,
                                                                  TileID(z: 12, x: 1, y: 0): 50_000]))
    }

    func testRefusalsFollowTheCapsAndFreeSpace() {
        XCTAssertNil(OfflinePlanner.refusal(tiles: 60_000, bytes: 2.4e9, freeBytes: nil, detail: .standard))
        XCTAssertEqual(OfflinePlanner.refusal(tiles: 60_001, bytes: 1e8, freeBytes: nil, detail: .high),
                       "Too large for High detail: pick a smaller area or Standard")
        XCTAssertEqual(OfflinePlanner.refusal(tiles: 100, bytes: 2.6e9, freeBytes: nil, detail: .standard),
                       "Too large: pick a smaller area")
        // 1 GB needs 1.25 GB + 500 MB free.
        XCTAssertNotNil(OfflinePlanner.refusal(tiles: 100, bytes: 1e9, freeBytes: 1_700_000_000, detail: .standard))
        XCTAssertNil(OfflinePlanner.refusal(tiles: 100, bytes: 1e9, freeBytes: 1_800_000_000, detail: .standard))
        // 500 MB still to come for a region already queued: (1 GB + 500 MB) × 1.25 + 500 MB.
        XCTAssertEqual(OfflinePlanner.refusal(tiles: 100, bytes: 1e9, freeBytes: 1_800_000_000, detail: .standard,
                                              queuedBytes: 5e8),
                       "Not enough free space: 2.4 GB needed with the downloads already queued, 1.8 GB free")
        XCTAssertEqual(OfflinePlanner.maxTiles, 60_000)
        XCTAssertEqual(OfflinePlanner.maxBytes, 2.5e9)
    }

    func testRequestsForRegionsAndTrailheads() throws {
        let box = GeoBounds(south: -30.68, west: 27.01, north: -28.57, east: 29.46)
        let region = try Self.jsonObject(OfflinePlanner.regionRequest(box, detail: .high))
        let area = try XCTUnwrap((region["areas"] as? [[String: Any]])?.first)
        XCTAssertEqual(area["bbox"] as? [Double], [-30.68, 27.01, -28.57, 29.46])
        XCTAssertEqual(area["zooms"] as? [Int], [8, 9, 10, 11, 12, 13])
        XCTAssertNil(region["trailhead"])

        // A located map's corners, TL TR BR BL, as the trailhead rule wants them.
        var map = Self.map(size: CGSize(width: 1_000, height: 800))
        XCTAssertNil(OfflinePlanner.trailheadRequest(for: map), "not located: nothing to keep")
        map.georef = Self.georef(topLeft: LatLon(lat: -25.80, lon: 27.35), metersPerPixel: 10)
        let corners = try XCTUnwrap(OfflinePlanner.corners(of: map))
        XCTAssertEqual(corners.count, 4)
        XCTAssertEqual(corners[0].lat, -25.80, accuracy: 1e-6)
        XCTAssertEqual(corners[0].lon, 27.35, accuracy: 1e-6)
        XCTAssertGreaterThan(corners[1].lon, corners[0].lon, "TR east of TL")
        XCTAssertLessThan(corners[2].lat, corners[1].lat, "BR south of TR")
        let trailhead = try Self.jsonObject(try XCTUnwrap(OfflinePlanner.trailheadRequest(for: map)))
        XCTAssertEqual(((trailhead["trailhead"] as? [String: Any])?["corners"] as? [[Double]])?.count, 4)
    }

    /// The core's plan (`mm_tile_plan`) covers the box at every zoom asked, in disjoint ranges.
    func testCorePlanCoversTheRegion() throws {
        let box = GeoBounds(south: -29.40, west: 29.30, north: -29.20, east: 29.55)
        let result = OfflinePlanner.plan(OfflinePlanner.regionRequest(box, detail: .standard))
        if case .failure(let failure) = result, failure.message.contains("not implemented") {
            throw XCTSkip("mm_tile_plan is not in this build of the core yet")
        }
        let plan = try result.get()
        XCTAssertEqual(plan.zooms.map(\.z).sorted(), [8, 9, 10, 11, 12])
        let tiles = OfflinePlanner.orderedTiles(plan)
        XCTAssertEqual(tiles.count, plan.tiles)
        XCTAssertEqual(plan.ranges.reduce(0) { $0 + $1.count }, plan.tiles, "ranges are disjoint")
        for z in 8...12 {
            for corner in [LatLon(lat: box.south, lon: box.west), LatLon(lat: box.north, lon: box.east), box.center] {
                XCTAssertTrue(plan.ranges.contains { $0.contains(TileMath.tile(for: corner, zoom: z)) }, "z\(z) misses \(corner)")
            }
        }
        XCTAssertGreaterThan(plan.bytesHigh, plan.bytes)
    }

    // MARK: - Pinned store

    func testPinnedTilesSurviveCacheEvictionAndStayWhileAnOwnerCoversThem() throws {
        let root = try makeTemporaryDirectory("pinned-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let cache = root.appendingPathComponent("dem")
        let regionID = UUID()
        let mapID = UUID()
        let regionRange = TileRange(z: 12, x0: 100, x1: 101, y0: 200, y1: 201)   // 4 tiles
        let mapRange = TileRange(z: 12, x0: 101, x1: 102, y0: 201, y1: 201)      // 2 tiles, one shared
        store.setRanges([regionRange], for: .region(regionID))
        store.setRanges([mapRange], for: .map(mapID))
        for t in regionRange.tiles + mapRange.tiles {
            try store.store(Self.png, for: t)
        }
        let shared = TileID(z: 12, x: 101, y: 201)
        XCTAssertTrue(store.covers(shared))
        XCTAssertNotNil(store.pinnedTile(shared))
        XCTAssertNil(store.pinnedTile(TileID(z: 12, x: 300, y: 300)), "not owned")
        XCTAssertNil(store.pinnedTile(TileID(z: 11, x: 100, y: 200)), "another zoom")

        // The cache fills up and is trimmed hard: pinned tiles live elsewhere and are untouched.
        let dem = DEMService(cacheDirectory: cache, session: Self.offlineSession(), cacheCapBytes: 10, pinned: store)
        for x in 0..<20 {
            let file = dem.cacheURL(for: TileID(z: 12, x: x, y: 7))
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Self.png.write(to: file)
        }
        XCTAssertEqual(DEMService.trimCache(at: cache, capBytes: 10), 20, "the whole cache goes")
        for t in regionRange.tiles + mapRange.tiles {
            XCTAssertEqual(dem.tileOnDevice(t), Self.png, "\(t) evicted with the cache")
        }

        // A new store (the next launch) reads the owners back.
        let reloaded = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        XCTAssertEqual(Set(reloaded.owners), [.region(regionID), .map(mapID)])
        XCTAssertEqual(reloaded.ranges(for: .map(mapID)), [mapRange])

        // Deleting the region keeps what the map still covers.
        reloaded.release(.region(regionID))
        XCTAssertFalse(reloaded.hasTile(TileID(z: 12, x: 100, y: 200)))
        XCTAssertFalse(reloaded.hasTile(TileID(z: 12, x: 101, y: 200)))
        XCTAssertTrue(reloaded.hasTile(shared), "the map still keeps it")
        XCTAssertTrue(reloaded.hasTile(TileID(z: 12, x: 102, y: 201)))
        reloaded.release(.map(mapID))
        XCTAssertTrue(reloaded.owners.isEmpty)
        XCTAssertEqual(reloaded.presence(of: [regionRange, mapRange]).tiles, 0)
        XCTAssertFalse(FileManager.default.fileExists(atPath: reloaded.manifestURL(for: .map(mapID)).path))
    }

    func testMovedTrailheadDropsTheTilesOnlyTheOldAreaNeeded() throws {
        let root = try makeTemporaryDirectory("pinned-move-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let map = TerrainOwner.map(UUID())
        let old = TileRange(z: 13, x0: 0, x1: 3, y0: 0, y1: 0)
        store.setRanges([old], for: map)
        for t in old.tiles { try store.store(Self.png, for: t) }
        // Located again, a little east: tiles 2 and 3 stay, 0 and 1 go.
        store.setRanges([TileRange(z: 13, x0: 2, x1: 5, y0: 0, y1: 0)], for: map)
        XCTAssertFalse(store.hasTile(TileID(z: 13, x: 0, y: 0)))
        XCTAssertFalse(store.hasTile(TileID(z: 13, x: 1, y: 0)))
        XCTAssertTrue(store.hasTile(TileID(z: 13, x: 2, y: 0)))
        XCTAssertTrue(store.hasTile(TileID(z: 13, x: 3, y: 0)))
    }

    func testAReleaseCutShortIsFinishedAtTheNextLaunch() throws {
        let root = try makeTemporaryDirectory("pinned-journal-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let owner = TerrainOwner.region(UUID())
        let range = TileRange(z: 11, x0: 5, x1: 6, y0: 5, y1: 6)
        store.setRanges([range], for: owner)
        for t in range.tiles { try store.store(Self.png, for: t) }
        // The app was killed right after the release renamed the manifest.
        let journal = store.ownersDirectory.appendingPathComponent(owner.fileStem + ".releasing.json")
        try FileManager.default.moveItem(at: store.manifestURL(for: owner), to: journal)

        let relaunched = PinnedTerrainStore(rootURL: root)
        XCTAssertTrue(relaunched.owners.isEmpty, "a release in progress owns nothing")
        XCTAssertEqual(relaunched.presence(of: [range]).tiles, 4, "its tiles are still on disk")
        relaunched.finishInterruptedReleases()
        XCTAssertEqual(relaunched.presence(of: [range]).tiles, 0)
        XCTAssertFalse(FileManager.default.fileExists(atPath: journal.path))
    }

    /// A tile that lands after its region was deleted (the download was on its way) is not kept:
    /// nothing stays on disk without an owner.
    func testATileWrittenAfterItsOwnerWasReleasedIsNotKept() throws {
        let root = try makeTemporaryDirectory("pinned-late-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let owner = TerrainOwner.region(UUID())
        let range = TileRange(z: 12, x0: 7, x1: 8, y0: 7, y1: 7)
        store.setRanges([range], for: owner)
        let first = TileID(z: 12, x: 7, y: 7)
        XCTAssertTrue(try store.store(Self.png, for: first))
        store.release(owner)
        XCTAssertFalse(store.hasTile(first), "released with its region")

        let late = TileID(z: 12, x: 8, y: 7)
        XCTAssertFalse(try store.store(Self.png, for: late), "no owner: not kept")
        XCTAssertFalse(store.hasTile(late))
        let file = root.appendingPathComponent("late.png")
        try Self.png.write(to: file)
        XCTAssertFalse(store.adopt(file, for: late))
        XCTAssertFalse(store.hasTile(late))
    }

    func testDownloadsOfCoveredTilesArePinnedOthersCached() async throws {
        let root = try makeTemporaryDirectory("pinned-dem-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let covered = TileID(z: 12, x: 2_400, y: 2_350)
        let other = TileID(z: 12, x: 9, y: 9)
        store.setRanges([TileRange(z: 12, x0: 2_400, x1: 2_400, y0: 2_350, y1: 2_350)], for: .region(UUID()))
        OfflineTestTileServer.reset()
        let dem = DEMService(cacheDirectory: root.appendingPathComponent("dem"), session: Self.serverSession(),
                             retryDelays: [0.01], pinned: store)
        let got = await dem.tiles([covered, other])
        XCTAssertEqual(got.count, 2)
        XCTAssertTrue(store.hasTile(covered), "a region's tile goes to the pinned store")
        XCTAssertFalse(FileManager.default.fileExists(atPath: dem.cacheURL(for: covered).path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: dem.cacheURL(for: other).path))
        XCTAssertFalse(store.hasTile(other))
    }

    // MARK: - Matching offline

    func testDeviceOnlyFetchesNeverDownload() async throws {
        let root = try makeTemporaryDirectory("device-only-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let pinnedTile = TileID(z: 11, x: 1_179, y: 1_175)
        let cachedTile = TileID(z: 11, x: 1_180, y: 1_175)
        let absent = TileID(z: 11, x: 1_181, y: 1_175)
        store.setRanges([TileRange(z: 11, x0: 1_179, x1: 1_179, y0: 1_175, y1: 1_175)], for: .region(UUID()))
        try store.store(Self.png, for: pinnedTile)
        OfflineTestTileServer.reset()
        let dem = DEMService(cacheDirectory: root.appendingPathComponent("dem"), session: Self.serverSession(), pinned: store)
        let file = dem.cacheURL(for: cachedTile)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Self.png.write(to: file)

        let local = await TerrainFetchScope.onDevice { await dem.tiles([pinnedTile, cachedTile, absent]) }
        XCTAssertEqual(Set(local.keys), [pinnedTile, cachedTile])
        let one = await TerrainFetchScope.onDevice { await dem.tile(absent) }
        XCTAssertNil(one)
        XCTAssertEqual(OfflineTestTileServer.getRequests, 0, "nothing downloaded inside the scope")
        // Outside the scope the absent tile is downloaded.
        let fetched = await dem.tiles([absent])
        XCTAssertEqual(fetched.count, 1)
        XCTAssertEqual(OfflineTestTileServer.getRequests, 1)
    }

    /// A search that starts offline outside a downloaded region: the first rounds find nothing
    /// (the loop's offline cutoff), the core then asks for tiles inside the region, and those are
    /// handed over from the pinned store instead of being marked missing.
    func testNeedDEMIsAnsweredFromPinnedTilesAfterAnOfflineCutoff() async throws {
        let root = try makeTemporaryDirectory("loop-pinned-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let region = TileRange(z: 11, x0: 1_176, x1: 1_178, y0: 1_174, y1: 1_175)
        store.setRanges([region], for: .region(UUID()))
        for t in region.tiles { try store.store(Self.png, for: t) }
        let dem = DEMService(cacheDirectory: root.appendingPathComponent("dem"), session: Self.offlineSession(), pinned: store)
        let outside = [[TileID(z: 11, x: 1_190, y: 1_190)], [TileID(z: 11, x: 1_191, y: 1_190)]]
        var added: [TileID] = []
        var missing: [TileID] = []
        let result = try await TerrainPullLoop().run(
            call: { i in
                if i < 2 { return GeorefResponse(status: "need_dem", needTiles: outside[i]) }
                if i == 2 { return GeorefResponse(status: "need_dem", needTiles: region.tiles + [TileID(z: 11, x: 1_192, y: 1_190)]) }
                return GeorefResponse(status: "ok")
            },
            fetch: { tiles in await dem.tiles(tiles, progress: nil) },
            add: { tile, _ in
                added.append(tile)
                return true
            },
            markMissing: { missing.append($0) },
            isCancelled: { false })
        XCTAssertEqual(result.end, .finished)
        XCTAssertEqual(result.cutoff, .offline, "two empty rounds: no network")
        XCTAssertEqual(Set(added), Set(region.tiles), "the region's tiles reach the core")
        XCTAssertEqual(result.tally.fromDevice, region.count)
        XCTAssertEqual(result.tally.fetched, region.count)
        XCTAssertEqual(Set(missing), Set(outside.flatMap { $0 } + [TileID(z: 11, x: 1_192, y: 1_190)]))
    }

    /// The Grootkloof sheet located from its contours with the network off and the terrain only
    /// in the pinned store (a downloaded region), as on a mountain without signal.
    func testLocatesFromPinnedTerrainWithTheNetworkOff() async throws {
        let (image, dpi) = try Fixtures.renderedPDF()
        let root = try makeTemporaryDirectory("offline-locate-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let source = try Fixtures.directory().appendingPathComponent("dem")
        var byZoom: [Int: [TileID]] = [:]
        var files: [(TileID, URL)] = []
        for name in try FileManager.default.contentsOfDirectory(atPath: source.path) where name.hasSuffix(".png") {
            let parts = name.dropLast(4).split(separator: "_").compactMap { Int($0) }
            guard parts.count == 3 else { continue }
            let t = TileID(z: parts[0], x: parts[1], y: parts[2])
            byZoom[t.z, default: []].append(t)
            files.append((t, source.appendingPathComponent(name)))
        }
        XCTAssertFalse(files.isEmpty)
        let ranges = byZoom.map { z, tiles in
            TileRange(z: z, x0: tiles.map(\.x).min() ?? 0, x1: tiles.map(\.x).max() ?? 0,
                      y0: tiles.map(\.y).min() ?? 0, y1: tiles.map(\.y).max() ?? 0)
        }
        store.setRanges(ranges, for: .region(UUID()))
        for (t, url) in files { try store.store(Data(contentsOf: url), for: t) }

        let cache = root.appendingPathComponent("dem")
        let dem = DEMService(cacheDirectory: cache, session: Self.offlineSession(), pinned: store)
        let service = GeoreferenceService(dem: dem, geocoder: AppPlaceGeocoder.make(), isOnline: { false })
        let gps = CLLocation(coordinate: CLLocationCoordinate2D(latitude: Fixtures.center.lat + 0.03, longitude: Fixtures.center.lon + 0.03),
                             altitude: 0, horizontalAccuracy: 10, verticalAccuracy: -1, timestamp: Date())
        let outcome = await service.locate(image: image, dpi: dpi, gps: gps, contourRGB: nil, searchRadiusKm: 20,
                                           knownText: []) { _, _ in }
        let g = try XCTUnwrap(outcome.georef, outcome.message)
        XCTAssertEqual(g.method, .contours)
        let truth = try GrootkloofTruth.load()
        let k = dpi.map { $0 / 300 } ?? Double(image.width) / truth.imageWidth
        let worst = truth.maxGroundError(of: g.pixelToMercator, scale: k)
        XCTAssertLessThan(worst, 60, "located offline with a max error of \(worst) m")
        XCTAssertGreaterThan(outcome.tally.fetched, 0)
        XCTAssertEqual(StorageUsage.directorySize(cache), 0, "nothing came from (or went to) the cache")
    }

    // MARK: - Download engine

    func testDownloadRetriesStopsOfflineAndResumesWithoutDownloadingAgain() async throws {
        let root = try makeTemporaryDirectory("engine-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let range = TileRange(z: 12, x0: 1_000, x1: 1_005, y0: 2_000, y1: 2_004)   // 30 tiles
        store.setRanges([range], for: .region(UUID()))
        let tiles = range.tiles
        let noData = tiles[4]
        // The last tile: never reached before the network goes.
        let flaky = tiles[29]
        var policy = TileDownloader.Policy()
        policy.concurrency = 4
        policy.retryDelays = [0.01, 0.01, 0.01]
        let downloader = TileDownloader(pinned: store, cacheDirectory: nil, session: Self.serverSession(), policy: policy)

        // The network goes after 12 answers; one tile has no data.
        OfflineTestTileServer.reset(offlineAfter: 12, missing: [noData])
        let first = await downloader.run(tiles) { _ in }
        XCTAssertEqual(first.stop, .offline)
        XCTAssertFalse(first.isComplete)
        XCTAssertFalse(first.cancelled)
        let onDisk = store.presence(of: [range]).tiles
        XCTAssertGreaterThan(onDisk, 0)
        XCTAssertLessThan(onDisk, 13)
        XCTAssertFalse(store.hasTile(flaky))

        // Back online: only the tiles not on disk are requested; one answers 503 twice first.
        OfflineTestTileServer.reset(failures: [flaky: 2], missing: [noData])
        let log = ProgressLog()
        let second = await downloader.run(tiles) { log.add($0) }
        XCTAssertTrue(second.isComplete)
        XCTAssertEqual(second.progress.done, 30, "29 tiles on disk and one without data")
        XCTAssertEqual(second.progress.failed, 0)
        XCTAssertEqual(OfflineTestTileServer.requests(for: flaky), 3, "two 503s, then the tile")
        XCTAssertEqual(OfflineTestTileServer.getRequests, 30 - onDisk + 2, "tiles already pinned are not downloaded again")
        XCTAssertEqual(store.presence(of: [range]).tiles, 29)
        XCTAssertFalse(store.hasTile(noData), "404: no data, nothing kept")
        XCTAssertEqual(log.last, second.progress, "the last report is the end")
    }

    func testTilesInTheCacheAreMovedNotDownloaded() async throws {
        let root = try makeTemporaryDirectory("engine-cache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let cache = root.appendingPathComponent("dem")
        let range = TileRange(z: 10, x0: 1, x1: 2, y0: 1, y1: 1)
        store.setRanges([range], for: .map(UUID()))
        let cachedTile = TileID(z: 10, x: 1, y: 1)
        let cacheService = DEMService(cacheDirectory: cache)
        let file = cacheService.cacheURL(for: cachedTile)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Self.png.write(to: file)
        OfflineTestTileServer.reset()
        let downloader = TileDownloader(pinned: store, cacheDirectory: cache, session: Self.serverSession())
        let summary = await downloader.run(range.tiles) { _ in }
        XCTAssertTrue(summary.isComplete)
        XCTAssertEqual(OfflineTestTileServer.getRequests, 1, "only the tile not in the cache")
        XCTAssertTrue(store.hasTile(cachedTile))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path), "moved, not copied")
    }

    func testCancellingPausesAndNothingIsWrittenForADeletedRegion() async throws {
        let root = try makeTemporaryDirectory("engine-cancel-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let owner = TerrainOwner.region(UUID())
        let range = TileRange(z: 13, x0: 0, x1: 39, y0: 0, y1: 4)   // 200 tiles
        store.setRanges([range], for: owner)
        OfflineTestTileServer.reset(delay: 0.05)
        let downloader = TileDownloader(pinned: store, cacheDirectory: nil, session: Self.serverSession())
        let task = Task.detached { await downloader.run(range.tiles) { _ in } }
        try await Task.sleep(nanoseconds: 150_000_000)
        task.cancel()
        let summary = await task.value
        XCTAssertTrue(summary.cancelled)
        XCTAssertNil(summary.stop)
        XCTAssertLessThan(summary.progress.done, 200)

        // Released (the region deleted) while tiles were still arriving: none is written.
        OfflineTestTileServer.reset(delay: 0.05)
        store.release(owner)
        let after = await downloader.run(range.tiles) { _ in }
        XCTAssertEqual(store.presence(of: [range]).tiles, 0)
        XCTAssertEqual(after.progress.done, 0)
    }

    func testWhatStopsADownload() {
        XCTAssertEqual(TileDownloader.stopReason(URLError(.notConnectedToInternet)), .offline)
        XCTAssertEqual(TileDownloader.stopReason(URLError(.dataNotAllowed)), .offline)
        XCTAssertNil(TileDownloader.stopReason(URLError(.timedOut)), "a timeout is retried, not a stop")
        let full = NSError(domain: NSCocoaErrorDomain, code: NSFileWriteOutOfSpaceError)
        XCTAssertTrue(TileDownloader.isOutOfSpace(full))
        XCTAssertEqual(TileDownloader.stopReason(full), .storageFull)
        XCTAssertTrue(TileDownloader.isOutOfSpace(NSError(domain: NSCocoaErrorDomain, code: 1,
                                                          userInfo: [NSUnderlyingErrorKey: NSError(domain: NSPOSIXErrorDomain, code: Int(ENOSPC))])))
        XCTAssertEqual(TileDownloadStop.wifi.pauseReason, .wifi)
        XCTAssertEqual(TileDownloadStop.offline.pauseReason, .network)
    }

    func testADownloadStopsBeforeTheDiskIsFull() async throws {
        let root = try makeTemporaryDirectory("engine-space-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = PinnedTerrainStore(rootURL: root)
        let range = TileRange(z: 13, x0: 0, x1: 49, y0: 0, y1: 4)   // 250 tiles
        store.setRanges([range], for: .region(UUID()))
        var policy = TileDownloader.Policy()
        policy.concurrency = 4

        // Already below the 500 MB floor: nothing is requested, the next run starts at the first tile.
        OfflineTestTileServer.reset()
        let full = TileDownloader(pinned: store, cacheDirectory: nil, session: Self.serverSession(), policy: policy,
                                  freeBytes: { Int64(400_000_000) })
        let refused = await full.run(range.tiles) { _ in }
        XCTAssertEqual(refused.stop, .storageFull)
        XCTAssertEqual(refused.stop?.pauseReason, .storageFull)
        XCTAssertEqual(refused.progress.cursor, 0)
        XCTAssertEqual(OfflineTestTileServer.getRequests, 0)

        // Space runs low partway (another download, photos): the run stops at the next look.
        OfflineTestTileServer.reset()
        let filling = TileDownloader(pinned: store, cacheDirectory: nil, session: Self.serverSession(), policy: policy,
                                     freeBytes: {
                                         OfflineTestTileServer.getRequests < 120 ? Int64(2_000_000_000) : Int64(100_000_000)
                                     })
        let stopped = await filling.run(range.tiles) { _ in }
        XCTAssertEqual(stopped.stop, .storageFull)
        XCTAssertGreaterThanOrEqual(stopped.progress.done, 100)
        XCTAssertLessThan(stopped.progress.done, 250)

        // Free space unknown: the download goes on to the end.
        let unknown = TileDownloader(pinned: store, cacheDirectory: nil, session: Self.serverSession(), policy: policy,
                                     freeBytes: { nil })
        let finished = await unknown.run(range.tiles) { _ in }
        XCTAssertTrue(finished.isComplete)
        XCTAssertEqual(store.presence(of: [range]).tiles, 250)
    }

    func testMeasuringAsksTheServerForSizes() async throws {
        let root = try makeTemporaryDirectory("measure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        OfflineTestTileServer.reset()
        let downloader = TileDownloader(pinned: PinnedTerrainStore(rootURL: root), cacheDirectory: nil, session: Self.serverSession())
        let tiles = (0..<5).map { TileID(z: 12, x: $0, y: 3) }
        let sizes = await downloader.measure(tiles)
        XCTAssertEqual(sizes.count, 5)
        XCTAssertTrue(sizes.values.allSatisfy { $0 == OfflineTestTileServer.headSize })
        XCTAssertEqual(OfflineTestTileServer.getRequests, 0, "HEAD only")
    }

    // MARK: - Region service

    func testWhatBlocksADownload() {
        XCTAssertEqual(OfflineRegionService.blocker(network: OfflineNetwork(online: false, expensive: false, constrained: false),
                                                    allowsCellular: true), .network)
        XCTAssertEqual(OfflineRegionService.blocker(network: OfflineNetwork(online: true, expensive: false, constrained: true),
                                                    allowsCellular: true), .lowDataMode)
        XCTAssertEqual(OfflineRegionService.blocker(network: OfflineNetwork(online: true, expensive: true, constrained: false),
                                                    allowsCellular: false), .wifi)
        XCTAssertNil(OfflineRegionService.blocker(network: OfflineNetwork(online: true, expensive: true, constrained: false),
                                                  allowsCellular: true))
        XCTAssertNil(OfflineRegionService.blocker(network: .wifi, allowsCellular: false))
    }

    /// A region downloads, the network goes, MapMe is killed; at the next launch it resumes
    /// where it stopped and finishes without downloading anything twice. Deleting it frees the
    /// tiles.
    @MainActor
    func testRegionDownloadPausesOfflineAndResumesAfterRelaunch() async throws {
        let root = try makeTemporaryDirectory("service-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let pinned = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let folder = root.appendingPathComponent("OfflineRegions")
        let range = TileRange(z: 12, x0: 2_300, x1: 2_305, y0: 2_340, y1: 2_344)   // 30 tiles
        let plan = Self.plan([range])
        func makeService() -> OfflineRegionService {
            OfflineRegionService(pinned: pinned, directory: folder, session: Self.serverSession(), cacheDirectory: nil,
                                 network: { .wifi }, retryDelays: [0.01], plan: { _ in .success(plan) })
        }

        OfflineTestTileServer.reset(offlineAfter: 10)
        let first = makeService()
        first.load()
        let id = first.download(name: " Test region ", bounds: GeoBounds(south: -29.4, west: 29.3, north: -29.2, east: 29.5),
                                detail: .standard, estimate: nil, allowsCellular: nil)
        XCTAssertEqual(first.region(id)?.name, "Test region")
        try await waitUntil { first.region(id)?.status == .paused }
        XCTAssertEqual(first.region(id)?.pauseReason, .network)
        XCTAssertEqual(first.region(id)?.message, "Waiting for network")
        XCTAssertNil(first.activeRegionID)
        let onDisk = pinned.presence(of: [range]).tiles
        XCTAssertGreaterThan(onDisk, 0)
        XCTAssertLessThan(onDisk, 30)
        first.flush()

        // The next launch: the saved region resumes once the network is back.
        OfflineTestTileServer.reset()
        let second = makeService()
        second.load()
        XCTAssertEqual(second.region(id)?.status, .paused)
        XCTAssertEqual(second.region(id)?.tilesTotal, 30)
        await second.resumeInterrupted()
        try await waitUntil { second.region(id)?.status == .complete }
        let region = try XCTUnwrap(second.region(id))
        XCTAssertEqual(region.tilesDone, 30)
        XCTAssertEqual(region.tilesFailed, 0)
        XCTAssertNil(region.message)
        XCTAssertNotNil(region.completedAt)
        XCTAssertEqual(region.bytesOnDisk, Int64(30 * Self.png.count))
        XCTAssertEqual(OfflineTestTileServer.getRequests, 30 - onDisk, "nothing downloaded twice")
        XCTAssertEqual(pinned.presence(of: [range]).tiles, 30)

        // Pause and resume by hand keep the region; delete frees its tiles.
        second.delete(id)
        XCTAssertNil(second.region(id))
        try await waitUntil { pinned.presence(of: [range]).tiles == 0 }
        second.flush()
        let third = makeService()
        third.load()
        XCTAssertTrue(third.regions.isEmpty, "the deletion was saved")
    }

    @MainActor
    func testUserPauseStaysPausedAcrossLaunches() async throws {
        let root = try makeTemporaryDirectory("service-pause-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let pinned = PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain"))
        let folder = root.appendingPathComponent("OfflineRegions")
        let plan = Self.plan([TileRange(z: 13, x0: 0, x1: 49, y0: 0, y1: 3)])   // 200 tiles
        OfflineTestTileServer.reset(delay: 0.03)
        let service = OfflineRegionService(pinned: pinned, directory: folder, session: Self.serverSession(), cacheDirectory: nil,
                                           network: { .wifi }, plan: { _ in .success(plan) })
        service.load()
        let id = service.download(name: "Slow", bounds: GeoBounds(south: 0, west: 0, north: 1, east: 1), detail: .high,
                                  estimate: nil, allowsCellular: nil)
        try await waitUntil { (service.region(id)?.tilesDone ?? 0) > 0 }
        service.pause(id)
        XCTAssertEqual(service.region(id)?.status, .paused)
        XCTAssertEqual(service.region(id)?.pauseReason, .user)
        XCTAssertNil(service.activeRegionID)
        service.flush()

        let relaunched = OfflineRegionService(pinned: pinned, directory: folder, session: Self.serverSession(),
                                              cacheDirectory: nil, network: { .wifi }, plan: { _ in .success(plan) })
        relaunched.load()
        await relaunched.resumeInterrupted()
        XCTAssertEqual(relaunched.region(id)?.status, .paused, "a pause the user chose is kept")
        relaunched.delete(id)
    }

    @MainActor
    func testOfflineEstimateIsTheTableAndOnlineIsMeasured() async throws {
        let root = try makeTemporaryDirectory("service-estimate-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let plan = TilePlan(tiles: 20, bytes: 20 * 44_000, bytesHigh: 20 * 117_000, zooms: [
            TilePlan.Zoom(z: 12, tiles: 16, bytes: 16 * 44_000, bytesHigh: 16 * 117_000,
                          ranges: [TileRange(z: 12, x0: 0, x1: 3, y0: 0, y1: 3)]),
            TilePlan.Zoom(z: 11, tiles: 4, bytes: 4 * 53_000, bytesHigh: 4 * 127_000,
                          ranges: [TileRange(z: 11, x0: 0, x1: 1, y0: 0, y1: 1)]),
        ], dropped: [])
        var online = false
        OfflineTestTileServer.reset()
        let service = OfflineRegionService(pinned: PinnedTerrainStore(rootURL: root), directory: root, session: Self.serverSession(),
                                           cacheDirectory: nil,
                                           network: { OfflineNetwork(online: online, expensive: false, constrained: false) },
                                           plan: { _ in .success(plan) })
        let box = GeoBounds(south: 0, west: 0, north: 1, east: 1)
        let offline = try await service.makeEstimate(box, detail: .standard).get()
        XCTAssertFalse(offline.isMeasured)
        XCTAssertEqual(offline.bytes, plan.bytesHigh, "offline: the high-resolution table, marked estimated")
        XCTAssertEqual(OfflineText.estimateLine(offline), "\u{2248} 2.3 MB \u{00B7} 20 tiles \u{00B7} estimated")

        online = true
        let measured = try await service.makeEstimate(box, detail: .standard).get()
        XCTAssertTrue(measured.isMeasured)
        XCTAssertEqual(measured.bytes, 20 * Double(OfflineTestTileServer.headSize), accuracy: 1)
        XCTAssertNil(measured.refusal)
        XCTAssertTrue(OfflineText.estimateLine(measured).hasSuffix("measured"))

        let failing = OfflineRegionService(pinned: PinnedTerrainStore(rootURL: root), directory: root, session: Self.serverSession(),
                                           cacheDirectory: nil, network: { .wifi },
                                           plan: { _ in .failure(OfflinePlanFailure(message: "bad box")) })
        let failed = await failing.estimate(box, detail: .standard)
        XCTAssertNil(failed)
    }

    @MainActor
    func testAnUnreadableRegionsFileIsMovedAsideNotOverwritten() throws {
        let root = try makeTemporaryDirectory("service-unreadable-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try Data("{ not json".utf8).write(to: root.appendingPathComponent("regions.json"))
        let service = OfflineRegionService(pinned: PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain")),
                                           directory: root, cacheDirectory: nil, network: { .wifi })
        service.load()
        XCTAssertTrue(service.regions.isEmpty)
        let names = try FileManager.default.contentsOfDirectory(atPath: root.path)
        XCTAssertTrue(names.contains { $0.hasPrefix("regions.unreadable-") && $0.hasSuffix(".json") }, "\(names)")
        XCTAssertFalse(names.contains("regions.json"))
    }

    @MainActor
    func testQueuedRegionsCountAgainstFreeSpace() throws {
        let root = try makeTemporaryDirectory("service-queued-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let plan = Self.plan([TileRange(z: 12, x0: 0, x1: 9, y0: 0, y1: 9)])
        let service = OfflineRegionService(pinned: PinnedTerrainStore(rootURL: root.appendingPathComponent("Terrain")),
                                           directory: root, session: Self.serverSession(), cacheDirectory: nil,
                                           network: { OfflineNetwork(online: false, expensive: false, constrained: false) },
                                           plan: { _ in .success(plan) })
        service.load()
        XCTAssertEqual(service.queuedBytes, 0)
        let estimate = OfflineEstimate(plan: plan, bytes: 1e9, isMeasured: true, refusal: nil)
        let id = service.download(name: "Big", bounds: GeoBounds(south: 0, west: 0, north: 1, east: 1), detail: .standard,
                                  estimate: estimate, allowsCellular: nil)
        XCTAssertEqual(service.region(id)?.status, .paused, "offline: it waits, and its gigabyte is spoken for")
        XCTAssertEqual(service.queuedBytes, 1e9, accuracy: 1)
        service.delete(id)
        XCTAssertEqual(service.queuedBytes, 0)
    }

    func testTheMapJustLocatedGetsItsTerrainFirstEvenWithTheSaverOn() {
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        // Queued at launch for a map located before (no request time).
        var backfilled = TrailheadTerrain(mapID: UUID())
        backfilled.updatedAt = t0.addingTimeInterval(-3_600)
        var earlier = TrailheadTerrain(mapID: UUID())
        earlier.updatedAt = t0.addingTimeInterval(-600)
        earlier.requestedAt = t0.addingTimeInterval(-600)
        var justLocated = TrailheadTerrain(mapID: UUID())
        justLocated.updatedAt = t0
        justLocated.requestedAt = t0
        let pending = [backfilled, earlier, justLocated]
        XCTAssertEqual(OfflineRegionService.nextTrailhead(pending, saverOn: false)?.mapID, justLocated.mapID)
        XCTAssertEqual(OfflineRegionService.nextTrailhead(pending, saverOn: true)?.mapID, justLocated.mapID)
        XCTAssertEqual(OfflineRegionService.nextTrailhead([backfilled, earlier], saverOn: false)?.mapID, earlier.mapID)
        // The battery saver holds back only maps queued without being located here.
        XCTAssertNil(OfflineRegionService.nextTrailhead([backfilled], saverOn: true))
        XCTAssertEqual(OfflineRegionService.nextTrailhead([backfilled], saverOn: false)?.mapID, backfilled.mapID)
        XCTAssertTrue(OfflineRegionService.waitsForSaver(backfilled, saverOn: true))
        XCTAssertFalse(OfflineRegionService.waitsForSaver(justLocated, saverOn: true))
        XCTAssertFalse(OfflineRegionService.waitsForSaver(backfilled, saverOn: false))
        // Done with tiles missing (a weak signal at the trailhead): worth another try, not pending.
        var holes = justLocated
        holes.status = .complete
        holes.tilesFailed = 3
        XCTAssertTrue(holes.hasMissingTiles)
        XCTAssertNil(OfflineRegionService.nextTrailhead([holes], saverOn: false))
        holes.tilesFailed = 0
        XCTAssertFalse(holes.hasMissingTiles)
    }

    func testTrailheadsDecodeWithoutARequestTime() throws {
        // Saved before `requestedAt` existed: treated as queued by the backfill.
        let old = """
        {"mapID":"00000000-0000-0000-0000-0000000000BB","status":"complete","tilesTotal":120,"tilesDone":118,
         "tilesFailed":2,"bytesOnDisk":9000000,"updatedAt":700000000}
        """
        let record = try JSONDecoder().decode(TrailheadTerrain.self, from: Data(old.utf8))
        XCTAssertNil(record.requestedAt)
        XCTAssertTrue(record.hasMissingTiles)
    }

    func testRegionsDecodeWithoutTheNewFields() throws {
        // Saved before the download engine's Optional fields existed.
        let old = """
        {"id":"00000000-0000-0000-0000-0000000000AA","name":"Lesotho","bounds":{"south":-30.68,"west":27.01,"north":-28.57,"east":29.46},
         "detail":"standard","createdAt":700000000,"status":"paused","tilesTotal":1150,"tilesDone":400,"tilesFailed":0,"bytesOnDisk":21000000}
        """
        let region = try JSONDecoder().decode(OfflineRegion.self, from: Data(old.utf8))
        XCTAssertNil(region.pauseReason)
        XCTAssertNil(region.allowsCellular)
        XCTAssertEqual(region.fraction, 400.0 / 1150, accuracy: 1e-9)
    }

    // MARK: - Wording

    @MainActor
    func testStatusLines() {
        var region = OfflineRegion(name: "Lesotho", bounds: GeoBounds(south: 0, west: 0, north: 1, east: 1), detail: .standard)
        region.status = .downloading
        region.tilesTotal = 2_475
        region.tilesDone = 1_204
        XCTAssertEqual(OfflineRegionRow.statusLine(region), "Standard \u{00B7} 1\u{00A0}204 of 2\u{00A0}475 tiles \u{00B7} 48 %")
        region.status = .paused
        region.pauseReason = .wifi
        XCTAssertEqual(OfflineRegionRow.statusLine(region), "Standard \u{00B7} Waiting for Wi-Fi \u{00B7} 48 %")
        region.status = .complete
        region.bytesOnDisk = 342_000_000
        region.completedAt = nil
        region.message = nil
        XCTAssertEqual(OfflineRegionRow.statusLine(region), "Standard \u{00B7} 342 MB")
        XCTAssertEqual(OfflineText.size(bytes: 1_211_000_000.0), "1.2 GB")
        XCTAssertEqual(OfflineText.size(bytes: 3_400_000.0), "3.4 MB")
        XCTAssertEqual(OfflineText.size(bytes: 400_000.0), "< 1 MB")
        XCTAssertEqual(OfflineText.tiles(1), "1 tile")

        var trailhead = TrailheadTerrain(mapID: UUID())
        XCTAssertEqual(TrailheadStatus.text(nil), "Not saved")
        trailhead.status = .downloading
        trailhead.tilesTotal = 200
        trailhead.tilesDone = 80
        XCTAssertEqual(TrailheadStatus.text(trailhead), "Saving\u{2026} 40 %")
        trailhead.status = .complete
        trailhead.bytesOnDisk = 8_200_000
        XCTAssertEqual(TrailheadStatus.text(trailhead), "Saved \u{00B7} 8.2 MB")
        XCTAssertEqual(TrailheadStatus.short(trailhead), "Terrain offline")
    }

    /// The region picker shows the other detail level's size to compare.
    @MainActor
    func testRegionPickerComparesTheOtherDetailLevel() {
        let plan = Self.plan([TileRange(z: 13, x0: 0, x1: 96, y0: 0, y1: 96)])   // 9 409 tiles
        var high = OfflineEstimate(plan: plan, bytes: 1_211_000_000, isMeasured: true, refusal: nil)
        XCTAssertEqual(RegionPickerView.comparisonLine(high, detail: .high),
                       "High detail: \u{2248} 1.2 GB \u{00B7} 9\u{00A0}409 tiles")
        high.refusal = "Too large for High detail: pick a smaller area or Standard"
        XCTAssertTrue(RegionPickerView.comparisonLine(high, detail: .high).hasSuffix("too large to download"))
        let standard = OfflineEstimate(plan: Self.plan([TileRange(z: 12, x0: 0, x1: 0, y0: 0, y1: 0)]), bytes: 44_000,
                                       isMeasured: false, refusal: nil)
        XCTAssertEqual(RegionPickerView.comparisonLine(standard, detail: .standard),
                       "Standard: < 1 MB \u{00B7} 1 tile", "a bound already: no \u{2248}")
    }

    func testSearchWiderOfflineNote() {
        XCTAssertEqual(OfflineTerrainInventory.searchNote(regionNames: ["Lesotho"], mapsWithTerrain: 3),
                       "Offline: searching the terrain on your iPhone (Lesotho, terrain around 3 maps).")
        XCTAssertTrue(OfflineTerrainInventory.searchNote(regionNames: [], mapsWithTerrain: 0).contains("no terrain is saved"))
        XCTAssertEqual(OfflineTerrainInventory.searchNote(regionNames: ["A", "B", "C", "D", "E"], mapsWithTerrain: 1),
                       "Offline: searching the terrain on your iPhone (A, B, C, 2 more regions, terrain around 1 map).")
    }

    func testMockPlaceArgument() throws {
        let place = try XCTUnwrap(OfflineMocks.parsePlace("Tasmania=-43.6,145.5,-40.7,148.5"))
        XCTAssertEqual(place.name, "Tasmania")
        XCTAssertEqual(place.bounds, GeoBounds(south: -43.6, west: 145.5, north: -40.7, east: 148.5))
        XCTAssertNil(OfflineMocks.parsePlace("Nowhere=1,2,3"))
        XCTAssertNil(OfflineMocks.parsePlace("Upside down=5,0,1,1"))
    }

    // MARK: - Helpers

    /// A tiny body with the PNG signature (what `DEMService.isPNG` checks).
    static let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D])

    static func plan(_ ranges: [TileRange]) -> TilePlan {
        var byZoom: [Int: [TileRange]] = [:]
        for r in ranges { byZoom[r.z, default: []].append(r) }
        let zooms = byZoom.keys.sorted().map { z -> TilePlan.Zoom in
            let n = byZoom[z, default: []].reduce(0) { $0 + $1.count }
            return TilePlan.Zoom(z: z, tiles: n, bytes: Double(n) * 50_000, bytesHigh: Double(n) * 110_000, ranges: byZoom[z] ?? [])
        }
        let total = zooms.reduce(0) { $0 + $1.tiles }
        return TilePlan(tiles: total, bytes: Double(total) * 50_000, bytesHigh: Double(total) * 110_000, zooms: zooms, dropped: [])
    }

    static func serverSession() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [OfflineTestTileServer.self]
        return URLSession(configuration: config)
    }

    /// Every request fails as on a phone without signal.
    static func offlineSession() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [NoSignalURLProtocol.self]
        return URLSession(configuration: config)
    }

    static func map(size: CGSize) -> MapDocument {
        MapDocument(id: UUID(), name: "Test", createdAt: Date(), lastOpenedAt: nil, sourceFileName: "source.pdf",
                    sourceKind: .pdf, pixelWidth: Int(size.width), pixelHeight: Int(size.height), renderDPI: nil,
                    tileSize: 256, levels: 1, georef: nil, georefMessage: nil)
    }

    /// North up, `metersPerPixel` ground metres (at the latitude) per pixel, `topLeft` at pixel (0, 0).
    static func georef(topLeft: LatLon, metersPerPixel: Double) -> Georeference {
        let origin = WebMercator.project(topLeft)
        let k = metersPerPixel / WebMercator.groundPerMercator(lat: topLeft.lat)
        return Georeference(pixelToMercator: Homography([k, 0, origin.x, 0, -k, origin.y, 0, 0, 1]), method: .manual,
                            confidence: 1, rmsMeters: 5)
    }

    static func jsonObject<T: Encodable>(_ value: T) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(value)) as? [String: Any])
    }

    @MainActor
    private func waitUntil(timeout: TimeInterval = 15, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline {
                XCTFail("Timed out after \(Int(timeout)) s")
                return
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

// MARK: - Fakes

/// A fake Terrarium server for the download engine: answers GETs with a small PNG (after
/// `delay`), HEADs with `headSize`; a tile in `failures` answers 503 that many times first, one
/// in `missing` answers 404; after `offlineAfter` answered GETs every request fails as offline.
final class OfflineTestTileServer: URLProtocol {
    static let headSize: Int64 = 52_000

    private static let lock = NSLock()
    private static var answered = 0
    private static var gets: [String: Int] = [:]
    private static var offlineAfter: Int?
    private static var failures: [String: Int] = [:]
    private static var missing: Set<String> = []
    private static var delay: TimeInterval = 0

    static func reset(offlineAfter: Int? = nil, failures: [TileID: Int] = [:], missing: Set<TileID> = [], delay: TimeInterval = 0) {
        lock.withLock {
            answered = 0
            gets = [:]
            self.offlineAfter = offlineAfter
            self.failures = Dictionary(uniqueKeysWithValues: failures.map { (key($0.key), $0.value) })
            self.missing = Set(missing.map(key))
            self.delay = delay
        }
    }

    /// GET requests that reached the server (not counting those refused as offline).
    static var getRequests: Int { lock.withLock { gets.values.reduce(0, +) } }

    static func requests(for t: TileID) -> Int { lock.withLock { gets[key(t), default: 0] } }

    private static func key(_ t: TileID) -> String { "/\(t.z)/\(t.x)/\(t.y).png" }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    private enum Answer {
        case offline
        case status(Int, Data)
        case head
    }

    override func startLoading() {
        guard let url = request.url else { return }
        let path = url.path
        let tileKey = path.range(of: "/terrarium").map { String(path[$0.upperBound...]) } ?? path
        let isHead = request.httpMethod == "HEAD"
        let (answer, wait): (Answer, TimeInterval) = Self.lock.withLock {
            if isHead { return (.head, 0) }
            if let limit = Self.offlineAfter, Self.answered >= limit { return (.offline, 0) }
            Self.answered += 1
            Self.gets[tileKey, default: 0] += 1
            if let left = Self.failures[tileKey], left > 0 {
                Self.failures[tileKey] = left - 1
                return (.status(503, Data()), Self.delay)
            }
            if Self.missing.contains(tileKey) { return (.status(404, Data()), Self.delay) }
            return (.status(200, OfflineTests.png), Self.delay)
        }
        let respond = { [weak self] in
            guard let self else { return }
            switch answer {
            case .offline:
                self.client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
                return
            case .head:
                if let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                                  headerFields: ["Content-Length": "\(Self.headSize)"]) {
                    self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                }
            case .status(let code, let body):
                if let response = HTTPURLResponse(url: url, statusCode: code, httpVersion: "HTTP/1.1", headerFields: nil) {
                    self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                }
                if !body.isEmpty { self.client?.urlProtocol(self, didLoad: body) }
            }
            self.client?.urlProtocolDidFinishLoading(self)
        }
        if wait > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + wait, execute: respond)
        } else {
            respond()
        }
    }

    override func stopLoading() {}
}

/// Progress reports collected from the downloader's (concurrent) callback.
final class ProgressLog: @unchecked Sendable {
    private let lock = NSLock()
    private var reports: [TileDownloadProgress] = []

    func add(_ p: TileDownloadProgress) {
        lock.withLock { reports.append(p) }
    }

    var last: TileDownloadProgress? { lock.withLock { reports.last } }
}

/// Fails every request as a phone without signal does.
final class NoSignalURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
    }

    override func stopLoading() {}
}
