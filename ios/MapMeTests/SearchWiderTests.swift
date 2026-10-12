import CoreLocation
import Foundation
import XCTest
@testable import MapMe

/// The first search's radius setting, the `need_dem` loop, place-name hints, estimates and their
/// wording, and which buttons a failed locate offers ("Search wider").
final class SearchWiderTests: XCTestCase {
    /// No-break space, as `SearchText` groups thousands ("1 000 km").
    private let nbsp = "\u{00A0}"

    // MARK: - Settings

    func testSearchRadiusDefaultsTo50Km() throws {
        let (defaults, suite) = try Self.freshDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let settings = AppSettings(defaults: defaults, arguments: [])
        XCTAssertEqual(settings.searchRadiusKm, 50)
        settings.searchRadiusKm = 75
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: []).searchRadiusKm, 75)
    }

    func testOldDefaultOf20KmBecomes50OnceAndAChosen20StaysAfterwards() throws {
        let (defaults, suite) = try Self.freshDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        // Stored by a version whose default was 20 km (no version key).
        defaults.set(20.0, forKey: "searchRadiusKm")
        let migrated = AppSettings(defaults: defaults, arguments: [])
        XCTAssertEqual(migrated.searchRadiusKm, 50)
        XCTAssertEqual(defaults.double(forKey: "searchRadiusKm"), 50)
        XCTAssertEqual(defaults.integer(forKey: "searchRadiusVersion"), AppSettings.searchRadiusVersion)

        // Picked on purpose after the migration: kept.
        migrated.searchRadiusKm = 20
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: []).searchRadiusKm, 20)
    }

    func testOtherStoredRadiiAreKeptWithinTheSliderRange() {
        XCTAssertEqual(AppSettings.migratedSearchRadius(35, storedVersion: 0), 35)
        XCTAssertEqual(AppSettings.migratedSearchRadius(20, storedVersion: 2), 20)
        XCTAssertEqual(AppSettings.migratedSearchRadius(5, storedVersion: 0), 10, "the old slider went down to 5 km")
        XCTAssertEqual(AppSettings.migratedSearchRadius(300, storedVersion: 2), 100)
        XCTAssertEqual(AppSettings.migratedSearchRadius(.nan, storedVersion: 0), 50)
        XCTAssertEqual(AppSettings.searchRadiusRangeKm, 10...100)
    }

    // MARK: - The need_dem loop

    func testLoopAnswersEveryRoundWhileTheCoreMakesProgress() async throws {
        // Eight chunks: more than the five rounds the old loop allowed.
        let chunks = (0..<8).map { c in (0..<10).map { TileID(z: 10, x: c * 10 + $0, y: 7) } }
        var added: [TileID] = []
        var fetches = 0
        let result = try await TerrainPullLoop().run(
            call: { i in
                i < chunks.count
                    ? Self.needDEM(chunks[i], search: SearchStatus(radiusM: 1_000_000, searchedRadiusM: Double(i) * 100_000,
                                                                   tilesPlanned: 80, tilesFetched: i * 10, stage: "wide"))
                    : Self.failed("No match within 1 000 km of you.")
            },
            fetch: { tiles in
                fetches += 1
                return Self.serve(tiles)
            },
            add: { tile, _ in
                added.append(tile)
                return true
            },
            markMissing: { tile in XCTFail("\(tile) should not be missing") },
            isCancelled: { false })
        XCTAssertEqual(result.end, .finished)
        XCTAssertNil(result.cutoff)
        XCTAssertEqual(result.response?.status, "failed")
        XCTAssertEqual(fetches, 8)
        XCTAssertEqual(result.tally, TerrainPullLoop.Tally(calls: 9, rounds: 8, requested: 80, fetched: 80, missing: 0))
        XCTAssertEqual(Set(added), Set(chunks.flatMap { $0 }))
    }

    func testLoopStopsDownloadingAtTheTileCapAndLetsTheCoreFinish() async throws {
        var policy = TerrainPullLoop.Policy()
        policy.tileCap = 100
        policy.maxFinishingCalls = 5
        var fetched = 0
        var missing = 0
        // A core that always wants 30 more tiles.
        let result = try await TerrainPullLoop(policy: policy).run(
            call: { i in Self.needDEM((0..<30).map { TileID(z: 9, x: i * 30 + $0, y: 1) }) },
            fetch: { tiles in
                fetched += tiles.count
                return Self.serve(tiles)
            },
            add: { _, _ in true },
            markMissing: { _ in missing += 1 },
            isCancelled: { false })
        XCTAssertEqual(result.cutoff, .tileCap)
        XCTAssertEqual(result.end, .callLimit)
        XCTAssertEqual(fetched, 90, "three rounds of 30; a fourth would pass the cap of 100")
        XCTAssertEqual(result.tally.requested, 90)
        // Calls 3…7 are answered with "missing" (five finishing calls), call 8 ends the loop.
        XCTAssertEqual(result.tally.calls, 9)
        XCTAssertEqual(missing, 5 * 30)

        // A core that gives up once tiles are missing ends normally, cut off by the cap.
        var sawMissing = false
        let finished = try await TerrainPullLoop(policy: policy).run(
            call: { i in
                sawMissing ? Self.failed("No match.") : Self.needDEM((0..<60).map { TileID(z: 9, x: i * 60 + $0, y: 2) })
            },
            fetch: { Self.serve($0) },
            add: { _, _ in true },
            markMissing: { _ in sawMissing = true },
            isCancelled: { false })
        XCTAssertEqual(finished.end, .finished)
        XCTAssertEqual(finished.cutoff, .tileCap)
        XCTAssertEqual(finished.tally.requested, 60)
    }

    func testLoopStopsWhenCancelled() async throws {
        var cancelled = false
        var calls = 0
        var fetches = 0
        let result = try await TerrainPullLoop().run(
            call: { i in
                calls += 1
                if i == 1 { cancelled = true }
                return Self.needDEM([TileID(z: 11, x: 100 + i, y: 200)])
            },
            fetch: { tiles in
                fetches += 1
                return Self.serve(tiles)
            },
            add: { _, _ in true },
            markMissing: { _ in },
            isCancelled: { cancelled })
        XCTAssertEqual(result.end, .cancelled)
        XCTAssertEqual(calls, 2)
        XCTAssertEqual(fetches, 1, "nothing is downloaded after the cancel")
    }

    func testLoopStopsDownloadingWhenOffline() async throws {
        var fetches = 0
        let result = try await TerrainPullLoop().run(
            call: { i in i < 4 ? Self.needDEM([TileID(z: 11, x: i, y: 0)]) : Self.failed("Terrain data couldn't be downloaded.") },
            fetch: { _ in
                fetches += 1
                return [:]
            },
            add: { _, _ in true },
            markMissing: { _ in },
            isCancelled: { false })
        XCTAssertEqual(result.end, .finished)
        XCTAssertEqual(result.cutoff, .offline)
        XCTAssertEqual(fetches, 2, "two empty rounds in a row stop the downloads")
        XCTAssertEqual(result.tally.fetched, 0)
        XCTAssertEqual(result.tally.missing, 4)

        // Once tiles have come, empty rounds (terrain that does not exist) do not stop the search.
        var rounds = 0
        let patchy = try await TerrainPullLoop().run(
            call: { i in i < 5 ? Self.needDEM([TileID(z: 13, x: i, y: 0)]) : Self.failed("No match.") },
            fetch: { tiles in
                rounds += 1
                return rounds == 1 || rounds == 5 ? Self.serve(tiles) : [:]
            },
            add: { _, _ in true },
            markMissing: { _ in },
            isCancelled: { false })
        XCTAssertNil(patchy.cutoff)
        XCTAssertEqual(rounds, 5)
        XCTAssertEqual(patchy.tally.fetched, 2)
        XCTAssertEqual(patchy.tally.missing, 3)
    }

    func testLoopEndsWhenTheCoreKeepsAskingForTheSameTiles() async throws {
        let same = [TileID(z: 11, x: 1, y: 1), TileID(z: 11, x: 2, y: 1)]
        var fetches = 0
        let result = try await TerrainPullLoop().run(
            call: { _ in Self.needDEM(same) },
            fetch: { tiles in
                fetches += 1
                return Self.serve(tiles)
            },
            add: { _, _ in true },
            markMissing: { _ in },
            isCancelled: { false })
        XCTAssertEqual(result.end, .stalled)
        XCTAssertEqual(fetches, 1)
        XCTAssertEqual(result.tally.calls, 4, "one real round, then three stalled ones (the limit is two)")
    }

    func testSearchStatusProgress() {
        let start = SearchStatus(radiusM: 1_000_000, searchedRadiusM: 50_000, tilesPlanned: 400, tilesFetched: 100, stage: "wide")
        XCTAssertEqual(start.fraction ?? -1, 0.25, accuracy: 1e-9)
        XCTAssertTrue(start.advanced(since: nil))
        XCTAssertFalse(start.advanced(since: start))
        var farther = start
        farther.searchedRadiusM = 60_000
        XCTAssertTrue(farther.advanced(since: start))
        var refining = start
        refining.stage = "refine"
        XCTAssertTrue(refining.advanced(since: start))
        XCTAssertEqual(refining.phase, .refine)
        // Without a tile plan: the share of the circle's area.
        let byArea = SearchStatus(radiusM: 1_000_000, searchedRadiusM: 500_000)
        XCTAssertEqual(byArea.fraction ?? -1, 0.25, accuracy: 1e-9)
        XCTAssertNil(SearchStatus().fraction)
    }

    // MARK: - The service with a fake core

    func testWideSearchLooksUpNamesThenAnswersEveryRound() async throws {
        let here = LatLon(lat: -25.85, lon: 27.40)
        let engine = FakeEngine { request, index in
            if request.methods == ["grid"] {
                return GeorefResponse(status: "failed", message: "Couldn't find coordinate grid labels on this map.",
                                      hintQueries: ["CASTLE GORGE", "MAGALIESBERG", "LEGEND"])
            }
            switch index {
            case 1:
                return Self.needDEM((0..<10).map { TileID(z: 11, x: 1100 + $0, y: 1170) },
                                    search: SearchStatus(radiusM: 1_000_000, searchedRadiusM: 0, tilesPlanned: 30, tilesFetched: 0,
                                                         hintsTried: 0, stage: "hints"))
            case 2:
                return Self.needDEM((0..<20).map { TileID(z: 9, x: 280 + $0, y: 290) },
                                    search: SearchStatus(radiusM: 1_000_000, searchedRadiusM: 320_000, coarseZoom: 9,
                                                         tilesPlanned: 30, tilesFetched: 10, hintsTried: 2, stage: "wide"))
            default:
                return GeorefResponse(status: "failed",
                                      message: "No match within 1 000 km of you (searched 30 terrain tiles; tried 2 places named on the map)",
                                      hintQueries: ["CASTLE GORGE", "MAGALIESBERG", "LEGEND"],
                                      search: SearchStatus(radiusM: 1_000_000, searchedRadiusM: 1_000_000, coarseZoom: 9,
                                                           tilesPlanned: 30, tilesFetched: 30, hintsTried: 2, stage: "done"))
            }
        }
        let geocoder = MockGeocoder(results: [
            "Castle Gorge": [GeocodedPlace(name: "Castle Gorge", coordinate: Geodesy.destination(here, bearing: 90, distance: 300_000)),
                             GeocodedPlace(name: "Castle Gorge", coordinate: Geodesy.destination(here, bearing: 0, distance: 1_500_000))],
            "Magaliesberg": [GeocodedPlace(name: "Magaliesberg", coordinate: Geodesy.destination(here, bearing: 200, distance: 40_000))],
        ])
        let service = GeoreferenceService(dem: FakeTerrain(), geocoder: geocoder, isOnline: { true })
        var request = GeorefRequest()
        request.gps = GPSFix(lat: here.lat, lon: here.lon, accuracyM: 10)
        request.ocr = [OCRObservation(text: "CASTLE GORGE", bbox: [10, 10, 200, 30], confidence: 0.9)]
        request.searchRadiusM = 1_000_000
        request.maxTilesPerRound = 64
        let log = StageRecorder()
        let outcome = await service.run(engine: engine, request: request, wide: WideSearchOptions(useMapNames: true),
                                        reporter: MonotonicProgress { log.add($0, $1) })

        let requests = engine.requests
        XCTAssertEqual(requests.count, 4)
        XCTAssertEqual(requests.first?.methods, ["grid"], "the names come from a grid-only call first")
        let hints = requests[1].hints
        XCTAssertEqual(hints.map(\.name), ["Castle Gorge", "Magaliesberg"], "the place 1 500 km away is outside the radius")
        XCTAssertTrue(hints.allSatisfy { $0.source == .place && $0.radiusM == SearchHint.defaultRadiusM })
        XCTAssertTrue(requests.dropFirst().allSatisfy { $0.hints == hints && $0.methods == ["grid", "contours"] })
        XCTAssertEqual(Set(geocoder.queries), ["Castle Gorge", "Magaliesberg", "Legend"])
        XCTAssertEqual(engine.added.count, 30)

        XCTAssertNil(outcome.georef)
        XCTAssertEqual(outcome.failure, .noTerrainMatch)
        XCTAssertEqual(outcome.message, "No match within 1 000 km of you (searched 30 terrain tiles; tried 2 places named on the map)")
        XCTAssertEqual(outcome.hintQueries, ["CASTLE GORGE", "MAGALIESBERG", "LEGEND"])
        XCTAssertEqual(outcome.hintsSent, 2)
        XCTAssertEqual(outcome.radiusKm, 1_000)
        XCTAssertTrue(log.stages.contains(.lookingUpPlaces))
        XCTAssertTrue(log.labels.contains("Searched 320 of 1\(nbsp)000 km"), "\(log.labels)")
        XCTAssertTrue(log.labels.contains("Checking places named on the map…"))
        XCTAssertTrue(log.isMonotonic)
        XCTAssertEqual(log.lastFraction, 1, accuracy: 1e-9)
    }

    func testKnownNamesSkipTheGridOnlyCallAndOfflineSkipsTheLookups() async throws {
        let engine = FakeEngine { _, _ in GeorefResponse(status: "failed", message: "No match.") }
        let geocoder = MockGeocoder(results: [:])
        let service = GeoreferenceService(dem: FakeTerrain(), geocoder: geocoder, isOnline: { false })
        var request = GeorefRequest()
        request.gps = GPSFix(lat: -25.85, lon: 27.40, accuracyM: 10)
        request.ocr = [OCRObservation(text: "GROOTKLOOF", bbox: [0, 0, 100, 20], confidence: 0.9)]
        request.searchRadiusM = 250_000
        _ = await service.run(engine: engine, request: request,
                              wide: WideSearchOptions(useMapNames: true, knownHintQueries: ["GROOTKLOOF"]),
                              reporter: MonotonicProgress { _, _ in })
        XCTAssertEqual(engine.requests.map(\.methods), [["grid", "contours"]])
        XCTAssertTrue(geocoder.queries.isEmpty)
        XCTAssertTrue(engine.requests[0].hints.isEmpty)
    }

    func testOfflineTerrainIsReportedAsSuch() async throws {
        let engine = FakeEngine { request, index in
            index < 3 ? Self.needDEM([TileID(z: 11, x: index, y: 3)]) : Self.failed("Terrain data for your area couldn't be downloaded.")
        }
        let service = GeoreferenceService(dem: FakeTerrain(available: false), geocoder: MockGeocoder(results: [:]), isOnline: { true })
        var request = GeorefRequest()
        request.gps = GPSFix(lat: -25.85, lon: 27.40, accuracyM: 10)
        let outcome = await service.run(engine: engine, request: request, wide: nil, reporter: MonotonicProgress { _, _ in })
        XCTAssertEqual(outcome.failure, .terrainUnavailable)
        XCTAssertEqual(WideSearch.resultActions(failure: outcome.failure, radiusKm: outcome.radiusKm, canCalibrate: true),
                       [.calibrate, .openAnyway, .tryAgain])
        XCTAssertEqual(engine.missing.count, 3)
    }

    // MARK: - Outcome kinds and the buttons they offer

    func testFailureKinds() throws {
        let searched = GeorefResponse(status: "failed", message: "Couldn't match this map's contours to terrain within 50 km of you.",
                                      search: SearchStatus(radiusM: 50_000, searchedRadiusM: 50_000, tilesPlanned: 25,
                                                           tilesFetched: 25, stage: "done"))
        XCTAssertEqual(GeoreferenceService.failure(for: searched, hadPosition: true, loop: nil), .noTerrainMatch)
        XCTAssertEqual(GeoreferenceService.failure(for: searched, hadPosition: false, loop: nil), .noPosition)

        let noContours = GeorefResponse(status: "failed", message: "No contour lines found on this map. Pick the contour colour, or calibrate manually.",
                                        search: SearchStatus(tilesPlanned: 25, tilesFetched: 25))
        XCTAssertEqual(GeoreferenceService.failure(for: noContours, hadPosition: true, loop: nil), .noContours)

        // An older core: no search status, but it asked for terrain.
        let plain = GeorefResponse(status: "failed", message: "Couldn't match this map's contours to terrain within 20 km of you.")
        let downloaded = Self.loopResult(plain, tally: TerrainPullLoop.Tally(calls: 3, rounds: 2, requested: 40, fetched: 40))
        XCTAssertEqual(GeoreferenceService.failure(for: plain, hadPosition: true, loop: downloaded), .noTerrainMatch)
        XCTAssertEqual(GeoreferenceService.failure(for: plain, hadPosition: true, loop: nil), .other)

        let offline = Self.loopResult(plain, tally: TerrainPullLoop.Tally(calls: 2, rounds: 1, requested: 25, fetched: 0, missing: 25))
        XCTAssertEqual(GeoreferenceService.failure(for: plain, hadPosition: true, loop: offline), .terrainUnavailable)

        let capped = Self.loopResult(plain, cutoff: .tileCap,
                                     tally: TerrainPullLoop.Tally(calls: 50, rounds: 40, requested: 2_500, fetched: 2_480))
        XCTAssertEqual(GeoreferenceService.failure(for: plain, hadPosition: true, loop: capped), .stoppedAtTileLimit)

        XCTAssertEqual(GeoreferenceService.failure(for: nil, hadPosition: true, loop: nil), .engineError)
        var cancelled = Self.loopResult(plain)
        cancelled.end = .cancelled
        XCTAssertEqual(GeoreferenceService.failure(for: plain, hadPosition: true, loop: cancelled), .cancelled)
    }

    func testFailureMessagesSayWhatWasSearched() {
        let wide = GeoreferenceService.outcome(
            from: GeorefResponse(status: "failed"), hadGPS: true, searchRadiusKm: 1_000, hintsSent: 3,
            loop: Self.loopResult(GeorefResponse(status: "failed"),
                                  tally: TerrainPullLoop.Tally(calls: 40, rounds: 38, requested: 2_410, fetched: 2_400)),
            isWide: true)
        XCTAssertEqual(wide.failure, .noTerrainMatch)
        XCTAssertEqual(wide.message, "No match within 1\(nbsp)000 km of you (searched 2\(nbsp)410 terrain tiles; tried 3 places named on the map).")

        let capped = GeoreferenceService.outcome(
            from: GeorefResponse(status: "need_dem", search: SearchStatus(radiusM: 2_000_000, searchedRadiusM: 640_000)),
            hadGPS: true, searchRadiusKm: 2_000,
            loop: Self.loopResult(GeorefResponse(status: "need_dem"), cutoff: .tileCap,
                                  tally: TerrainPullLoop.Tally(calls: 45, rounds: 40, requested: 2_500, fetched: 2_500)),
            isWide: true)
        XCTAssertEqual(capped.failure, .stoppedAtTileLimit)
        XCTAssertEqual(capped.message,
                       "Stopped after downloading 2\(nbsp)500 terrain tiles (searched 640 of 2\(nbsp)000 km). Try a shorter distance, or calibrate manually.")

        let noGPS = GeoreferenceService.outcome(from: GeorefResponse(status: "failed", message: "No coordinate labels found."),
                                                hadGPS: false, searchRadiusKm: 50)
        XCTAssertEqual(noGPS.failure, .noPosition)
        XCTAssertTrue(noGPS.message.contains("Turn on location"))

        // After a wide search the core's own message wins when it has one.
        let core = GeoreferenceService.outcome(from: GeorefResponse(status: "failed", message: "No match within 250 km of you.",
                                                                    search: SearchStatus(tilesPlanned: 160)),
                                               hadGPS: true, searchRadiusKm: 250, isWide: true)
        XCTAssertEqual(core.message, "No match within 250 km of you.")

        // After the first search the app's words lead to the buttons on offer.
        let near = GeoreferenceService.outcome(
            from: GeorefResponse(status: "failed",
                                 message: "Couldn't match this map's contours to terrain within 50 km of you. Try calibrating manually or pick the contour colour.",
                                 search: SearchStatus(radiusM: 50_000, searchedRadiusM: 50_000, tilesPlanned: 25, tilesFetched: 25)),
            hadGPS: true, searchRadiusKm: 50)
        XCTAssertEqual(near.failure, .noTerrainMatch)
        XCTAssertEqual(near.message,
                       "This map's contour lines didn't match the terrain within 50 km of you. Search wider, or calibrate it manually.")

        // No position, but names on the map: the message points at them.
        let named = GeoreferenceService.outcome(from: GeorefResponse(status: "failed", message: "No coordinate labels found.",
                                                                     hintQueries: ["CASTLE GORGE"]),
                                                hadGPS: false, searchRadiusKm: 50)
        XCTAssertEqual(named.failure, .noPosition)
        XCTAssertTrue(named.message.contains("Turn on location") && named.message.contains("Search wider"), named.message)
    }

    func testResultActionsFollowTheOutcome() {
        // First (near) search missed: lead with "Search wider…".
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 50, canCalibrate: true),
                       [.searchWider, .calibrate, .openAnyway])
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 20, canCalibrate: true),
                       [.searchWider, .calibrate, .openAnyway])
        // A wide search missed: calibrate first, still offer a wider one below 2 000 km.
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 1_000, canCalibrate: true),
                       [.calibrate, .searchWider, .openAnyway])
        XCTAssertEqual(WideSearch.resultActions(failure: .stoppedAtTileLimit, radiusKm: 1_000, canCalibrate: true),
                       [.calibrate, .searchWider, .openAnyway])
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 2_000, canCalibrate: true),
                       [.calibrate, .openAnyway, .tryAgain])
        // Not a terrain miss: no wider search.
        for failure: GeorefFailure? in [.noPosition, .noContours, .terrainUnavailable, .engineError, .other, nil] {
            XCTAssertEqual(WideSearch.resultActions(failure: failure, radiusKm: 50, canCalibrate: true),
                           [.calibrate, .openAnyway, .tryAgain], "\(String(describing: failure))")
        }
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 50, canCalibrate: false), [.openAnyway])
        // A first search set to 100 km in Settings still leads with "Search wider…".
        XCTAssertEqual(WideSearch.resultActions(failure: .noTerrainMatch, radiusKm: 100, canCalibrate: true),
                       [.searchWider, .calibrate, .openAnyway])
        // No position, but names on the map to look up.
        XCTAssertEqual(WideSearch.resultActions(failure: .noPosition, radiusKm: 50, canCalibrate: true, hasMapNames: true),
                       [.calibrate, .searchWider, .openAnyway])
        XCTAssertEqual(WideSearch.resultActions(failure: .noContours, radiusKm: 50, canCalibrate: true, hasMapNames: true),
                       [.calibrate, .openAnyway, .tryAgain])
        // The library menu offers it unless the map has no contours or could not be read.
        let kinds: [GeorefFailure] = [.noTerrainMatch, .stoppedAtTileLimit, .noPosition, .noContours, .terrainUnavailable,
                                      .engineError, .cancelled, .other]
        XCTAssertEqual(kinds.filter(\.widerSearchMayHelp),
                       [.noTerrainMatch, .stoppedAtTileLimit, .noPosition, .terrainUnavailable, .cancelled, .other])
    }

    // MARK: - Place-name hints

    func testHintsKeepPlacesWithinTheRadiusNearestFirstPerName() {
        let here = LatLon(lat: -25.85, lon: 27.40)
        func place(_ name: String, _ km: Double, _ bearing: Double = 45) -> GeocodedPlace {
            GeocodedPlace(name: name, coordinate: Geodesy.destination(here, bearing: bearing, distance: km * 1000))
        }
        let results: [(query: String, places: [GeocodedPlace])] = [
            ("Castle Gorge", [place("Castle Gorge", 900), place("Castle Gorge", 120), place("Castle Gorge", 1_400)]),
            ("Magaliesberg", [place("Magaliesberg", 60, 300)]),
            ("Nowhere", []),
            // 2 km from the Castle Gorge found first: adds nothing.
            ("Castle Gorge Hut", [GeocodedPlace(name: "Hut", coordinate: Geodesy.destination(place("x", 120).coordinate,
                                                                                            bearing: 0, distance: 2_000))]),
        ]
        let hints = HintGeocoder.hints(from: results, near: here, searchRadiusM: 1_000_000)
        XCTAssertEqual(hints.map(\.name), ["Castle Gorge", "Magaliesberg", "Castle Gorge"])
        XCTAssertEqual(Geodesy.distance(here, hints[0].center) / 1000, 120, accuracy: 1, "nearest first")
        XCTAssertEqual(Geodesy.distance(here, hints[2].center) / 1000, 900, accuracy: 1, "second results after every name's first")
        XCTAssertTrue(hints.allSatisfy { $0.source == .place && $0.radiusM == 15_000 })

        // Within 250 km only the nearer ones are left.
        XCTAssertEqual(HintGeocoder.hints(from: results, near: here, searchRadiusM: 250_000).map(\.name),
                       ["Castle Gorge", "Magaliesberg"])
        // At most one per name, at most two in all.
        XCTAssertEqual(HintGeocoder.hints(from: results, near: here, searchRadiusM: 1_000_000, maxResultsPerQuery: 1, maxHints: 2).count, 2)
        // Without a fix: anywhere, in the order Apple Maps gave.
        let anywhere = HintGeocoder.hints(from: results, near: nil, searchRadiusM: 50_000, maxResultsPerQuery: 1)
        XCTAssertEqual(Geodesy.distance(here, anywhere[0].center) / 1000, 900, accuracy: 1)
    }

    func testQueriesAreCleanedBeforeLookup() {
        XCTAssertEqual(HintGeocoder.cleanQueries(["CASTLE  GORGE", "castle gorge", " Magaliesberg ", "N1", "", "LOSKOP DAM"]),
                       ["Castle Gorge", "Magaliesberg", "Loskop Dam"])
    }

    func testLookupsRunAFewAtATimeAndSlowOnesAreDropped() async {
        let here = LatLon(lat: -25.85, lon: 27.40)
        let near = GeocodedPlace(name: "x", coordinate: Geodesy.destination(here, bearing: 10, distance: 30_000))
        let geocoder = MockGeocoder(results: ["Alpha": [near], "Bravo": [near], "Charlie": [near], "Delta": [near],
                                              "Slow": [near]],
                                    delay: 0.05, hang: ["Slow"])
        var hinter = HintGeocoder(geocoder: geocoder)
        hinter.maxConcurrent = 2
        hinter.timeoutSeconds = 0.4
        hinter.mergeDistanceM = 0
        let started = Date()
        let hints = await hinter.hints(for: ["SLOW", "ALPHA", "BRAVO", "CHARLIE", "DELTA"], near: here, searchRadiusM: 100_000)
        XCTAssertLessThan(Date().timeIntervalSince(started), 3, "a hanging lookup must not hold up the rest")
        XCTAssertEqual(hints.map(\.name), ["Alpha", "Bravo", "Charlie", "Delta"])
        XCTAssertLessThanOrEqual(geocoder.maxInFlight, 2)
        XCTAssertEqual(Set(geocoder.queries), ["Slow", "Alpha", "Bravo", "Charlie", "Delta"])

        var offline = HintGeocoder(geocoder: geocoder)
        offline.isOnline = { false }
        let none = await offline.hints(for: ["ALPHA"], near: here, searchRadiusM: 100_000)
        XCTAssertTrue(none.isEmpty)
    }

    func testTimeoutDoesNotWaitForALookupThatIgnoresCancellation() async {
        let started = Date()
        let value: Int? = await HintGeocoder.withTimeout(0.2, grace: 0.2) {
            // Awaiting a detached task does not pass the cancel on: like an MKLocalSearch that
            // never calls back.
            await Task.detached { () -> Int in
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                return 7
            }.value
        }
        XCTAssertNil(value)
        XCTAssertLessThan(Date().timeIntervalSince(started), 1.5)

        let quick: Int? = await HintGeocoder.withTimeout(2) { 42 }
        XCTAssertEqual(quick, 42)
        let failing: Int? = await HintGeocoder.withTimeout(2) { () throws -> Int in throw URLError(.notConnectedToInternet) }
        XCTAssertNil(failing)
    }

    // MARK: - Estimates and wording

    func testEstimateWording() {
        XCTAssertEqual(SearchText.size(bytes: 400_000), "Less than 1 MB")
        XCTAssertEqual(SearchText.size(bytes: 4_400_000), "About 4 MB")
        XCTAssertEqual(SearchText.size(bytes: 45_000_000), "About 45 MB")
        XCTAssertEqual(SearchText.size(bytes: 47_000_000), "About 45 MB")
        XCTAssertEqual(SearchText.size(bytes: 123_000_000), "About 120 MB")
        XCTAssertEqual(SearchText.size(bytes: 1_240_000_000), "About 1.2 GB")

        XCTAssertEqual(SearchText.duration(seconds: 30), "under a minute")
        XCTAssertEqual(SearchText.duration(seconds: 100), "a minute or two")
        XCTAssertEqual(SearchText.duration(seconds: 200), "a few minutes")
        XCTAssertEqual(SearchText.duration(seconds: 1_500), "about 25 minutes")
        XCTAssertEqual(SearchText.duration(seconds: 4_000), "about an hour")
        XCTAssertEqual(SearchText.duration(seconds: 10_000), "about 3 hours")

        XCTAssertEqual(SearchText.estimateLine(SearchEstimate(coarseZoom: 9, tiles: 630, approxBytes: 45_000_000, approxSeconds: 200)),
                       "About 45 MB of terrain data, a few minutes. Wi-Fi recommended.")
        XCTAssertEqual(SearchText.estimateLine(SearchEstimate(coarseZoom: 10, tiles: 160, approxBytes: 6_000_000, approxSeconds: 40)),
                       "About 6 MB of terrain data, under a minute.")
        XCTAssertEqual(SearchText.rowDetail(SearchEstimate(coarseZoom: 10, tiles: 160, approxBytes: 6_000_000, approxSeconds: 40)),
                       "about 6 MB")

        XCTAssertTrue(SearchText.needsCellularConfirmation(bytes: 60_000_000, isExpensive: true))
        XCTAssertFalse(SearchText.needsCellularConfirmation(bytes: 60_000_000, isExpensive: false))
        XCTAssertFalse(SearchText.needsCellularConfirmation(bytes: 40_000_000, isExpensive: true))
        XCTAssertFalse(SearchText.needsCellularConfirmation(bytes: nil, isExpensive: true))
    }

    func testDistancesInKilometresAndMiles() {
        XCTAssertEqual(SearchText.grouped(999), "999")
        XCTAssertEqual(SearchText.grouped(2_410), "2\(nbsp)410")
        XCTAssertEqual(SearchText.grouped(1_234_567), "1\(nbsp)234\(nbsp)567")
        XCTAssertEqual(SearchText.distance(km: 50, units: .metric), "50 km")
        XCTAssertEqual(SearchText.distance(km: 1_000, units: .metric), "1\(nbsp)000 km")
        XCTAssertEqual(SearchText.distance(km: 1_000, units: .imperial), "621 mi")
        XCTAssertEqual(WideSearch.presetsKm(units: .imperial).map { SearchText.distance(km: $0, units: .imperial) },
                       ["60 mi", "150 mi", "300 mi", "600 mi"])
        XCTAssertEqual(WideSearch.presetsKm(units: .metric), [100, 250, 500, 1_000])

        XCTAssertEqual(SearchText.searched(km: 318.7, of: 1_000, units: .metric), "Searched 320 of 1\(nbsp)000 km")
        XCTAssertEqual(SearchText.searched(km: 42.4, of: 50, units: .metric), "Searched 42 of 50 km")
        XCTAssertEqual(SearchText.searched(km: 1_200, of: 1_000, units: .metric), "Searched 1\(nbsp)000 of 1\(nbsp)000 km")
        XCTAssertEqual(SearchText.searched(km: 500, of: 1_000, units: .imperial), "Searched 310 of 621 mi")

        let wide = SearchStatus(radiusM: 1_000_000, searchedRadiusM: 320_000, stage: "wide")
        XCTAssertEqual(SearchText.progressHeadline(wide, radiusKm: 1_000, units: .metric), "Searched 320 of 1\(nbsp)000 km")
        XCTAssertEqual(SearchText.progressHeadline(SearchStatus(stage: "hints"), radiusKm: 1_000, units: .metric),
                       "Checking places named on the map")
        XCTAssertEqual(SearchText.progressHeadline(nil, radiusKm: 1_000, units: .metric), "Searching up to 1\(nbsp)000 km away")
        XCTAssertEqual(GeorefStage.searching(wide).label(units: .metric), "Searched 320 of 1\(nbsp)000 km")
        XCTAssertEqual(GeorefStage.searching(SearchStatus(stage: "near")).label, "Searching near you…")
        XCTAssertEqual(GeorefStage.searching(SearchStatus(stage: "refine")).label, "Checking a likely match…")
    }

    func testCustomRadiusAndDefaults() {
        XCTAssertEqual(WideSearch.customRange(units: .metric), 10...2_000)
        XCTAssertEqual(WideSearch.customRange(units: .imperial), 7...1_242)
        XCTAssertNil(WideSearch.customKm(5, units: .metric))
        XCTAssertNil(WideSearch.customKm(2_001, units: .metric))
        XCTAssertNil(WideSearch.customKm(nil, units: .metric))
        XCTAssertEqual(WideSearch.customKm(1_000, units: .metric), 1_000)
        XCTAssertEqual(WideSearch.customKm(600, units: .imperial) ?? 0, 965.6, accuracy: 0.1)
        XCTAssertEqual(WideSearch.customKm(1_242, units: .imperial) ?? 0, 1_998.8, accuracy: 0.1)

        XCTAssertEqual(WideSearch.defaultPresetKm(after: 50, units: .metric), 250)
        XCTAssertEqual(WideSearch.defaultPresetKm(after: nil, units: .metric), 250)
        XCTAssertEqual(WideSearch.defaultPresetKm(after: 250, units: .metric), 1_000)
        XCTAssertNil(WideSearch.defaultPresetKm(after: 1_000, units: .metric))
        XCTAssertEqual(WideSearch.defaultPresetKm(after: 50, units: .imperial) ?? 0, 150 * WideSearch.kmPerMile, accuracy: 1e-9)

        XCTAssertEqual(WideSearch.tileCap(for: nil), 2_500)
        XCTAssertEqual(WideSearch.tileCap(for: SearchEstimate(coarseZoom: 10, tiles: 160, approxBytes: 1, approxSeconds: 1)), 2_500)
        XCTAssertEqual(WideSearch.tileCap(for: SearchEstimate(coarseZoom: 9, tiles: 4_000, approxBytes: 1, approxSeconds: 1)), 6_000)

        XCTAssertEqual(SearchWiderSheet.namesFooter([]),
                       "Looks up the places named on the map with Apple Maps and checks there first.")
        XCTAssertEqual(SearchWiderSheet.namesFooter(["CASTLE GORGE"]),
                       "Looks up \u{201C}Castle Gorge\u{201D} with Apple Maps and checks those places first.")
        XCTAssertEqual(SearchWiderSheet.namesFooter(["CASTLE GORGE", "MAGALIESBERG", "LOSKOP", "ELANDS"]),
                       "Looks up \u{201C}Castle Gorge\u{201D}, \u{201C}Magaliesberg\u{201D} and 2 more with Apple Maps and checks those places first.")
        XCTAssertEqual(SearchWiderSheet.displayValue(1_000, units: .metric), 1_000)
        XCTAssertEqual(SearchWiderSheet.displayValue(60 * WideSearch.kmPerMile, units: .imperial), 60)
    }

    // MARK: - Wire format

    func testRequestCarriesRadiusHintsAndRoundSize() throws {
        var request = GeorefRequest()
        XCTAssertEqual(request.searchRadiusM, 50_000)
        var json = try Self.jsonObject(request)
        XCTAssertEqual(json["search_radius_m"] as? Double, 50_000)
        XCTAssertEqual((json["hints"] as? [Any])?.count, 0)
        XCTAssertNil(json["max_tiles_per_round"])

        request.searchRadiusM = 1_000_000
        request.maxTilesPerRound = 64
        request.hints = [SearchHint(lat: -25.8, lon: 27.4, source: .place, name: "Castle Gorge")]
        json = try Self.jsonObject(request)
        XCTAssertEqual(json["max_tiles_per_round"] as? Int, 64)
        let hint = try XCTUnwrap((json["hints"] as? [[String: Any]])?.first)
        XCTAssertEqual(hint["lat"] as? Double, -25.8)
        XCTAssertEqual(hint["lon"] as? Double, 27.4)
        XCTAssertEqual(hint["radius_m"] as? Double, 15_000)
        XCTAssertEqual(hint["source"] as? String, "place")
        XCTAssertEqual(hint["name"] as? String, "Castle Gorge")

        let estimate = try Self.jsonObject(SearchEstimateRequest(radiusM: 250_000, gps: nil))
        XCTAssertTrue(estimate["gps"] is NSNull, "no fix is an explicit null")
        XCTAssertEqual(estimate["radius_m"] as? Double, 250_000)
    }

    func testResponseReadsNamesAndSearchStatusLeniently() throws {
        let response = try Self.decode("""
        {"status":"need_dem","need_tiles":[{"z":9,"x":280,"y":290}],
         "hint_queries":["CASTLE GORGE","MAGALIESBERG"],
         "search":{"radius_m":1000000,"searched_radius_m":320000.0,"coarse_zoom":9,"tiles_planned":2410.0,
                   "tiles_fetched":640,"hints_tried":3,"stage":"wide"}}
        """)
        XCTAssertEqual(response.hintQueries, ["CASTLE GORGE", "MAGALIESBERG"])
        let search = try XCTUnwrap(response.search)
        XCTAssertEqual(search.tilesPlanned, 2_410)
        XCTAssertEqual(search.tilesFetched, 640)
        XCTAssertEqual(search.coarseZoom, 9)
        XCTAssertEqual(search.searchedRadiusM, 320_000)
        XCTAssertEqual(search.phase, .wide)

        // A malformed search status or name list is dropped, not fatal.
        let odd = try Self.decode(#"{"status":"failed","message":"x","search":"oops","hint_queries":{"a":1}}"#)
        XCTAssertNil(odd.search)
        XCTAssertNil(odd.hintQueries)
        XCTAssertEqual(odd.message, "x")
    }

    func testSearchEstimateDecoding() {
        let ok = MapMeCore.decodeSearchEstimate(#"{"status":"ok","coarse_zoom":9,"tiles":630.0,"approx_bytes":4.5e7,"approx_seconds":200}"#)
        XCTAssertEqual(ok, SearchEstimate(coarseZoom: 9, tiles: 630, approxBytes: 45_000_000, approxSeconds: 200))
        XCTAssertNil(MapMeCore.decodeSearchEstimate(#"{"status":"failed","message":"bad radius"}"#))
        XCTAssertNil(MapMeCore.decodeSearchEstimate(#"{"status":"ok","tiles":-1,"approx_bytes":1,"approx_seconds":1}"#))
        XCTAssertNil(MapMeCore.decodeSearchEstimate(#"{"status":"ok","tiles":10}"#))
        XCTAssertNil(MapMeCore.decodeSearchEstimate("not json"))
    }

    // MARK: - Stored state

    func testLocateAttemptIsStoredAndOldMetadataStillDecodes() throws {
        var doc = MapDocument(id: UUID(), name: "Far away", createdAt: Date(timeIntervalSince1970: 1_700_000_000), lastOpenedAt: nil,
                              sourceFileName: "source.jpg", sourceKind: .image, pixelWidth: 100, pixelHeight: 80, renderDPI: nil,
                              tileSize: 512, levels: 1, georef: nil, georefMessage: "Not found")
        let encoder = MapLibrary.makeEncoder()
        let decoder = MapLibrary.makeDecoder()
        XCTAssertNil(try decoder.decode(MapDocument.self, from: encoder.encode(doc)).locateAttempt)

        doc.locateAttempt = LocateAttempt(failure: .noTerrainMatch, searchedRadiusKm: 50, hintQueries: ["CASTLE GORGE"])
        let back = try decoder.decode(MapDocument.self, from: encoder.encode(doc))
        XCTAssertEqual(back.locateAttempt, doc.locateAttempt)

        // Written by a newer version: unknown kinds and missing fields do not hide the map.
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(doc)) as? [String: Any])
        object["locateAttempt"] = ["failure": "somethingNew"]
        let newer = try decoder.decode(MapDocument.self, from: JSONSerialization.data(withJSONObject: object))
        XCTAssertEqual(newer.locateAttempt, LocateAttempt(failure: .other, searchedRadiusKm: nil, hintQueries: []))
    }

    func testOCRCacheIsTiedToTheRasterSize() throws {
        let dir = try makeTemporaryDirectory("ocr-cache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent(OCRCache.fileName)
        let observations = [OCRObservation(text: "25°49'00\"S", bbox: [116, 449, 36, 185], confidence: 0.92)]
        try OCRCache(pixelWidth: 3_507, pixelHeight: 4_960, observations: observations).write(to: url)
        XCTAssertEqual(OCRCache.load(from: url, width: 3_507, height: 4_960), observations)
        XCTAssertNil(OCRCache.load(from: url, width: 3_000, height: 4_960))
        XCTAssertNil(OCRCache.load(from: dir.appendingPathComponent("missing.json"), width: 3_507, height: 4_960))
    }

    // MARK: - Terrain cache and downloads

    func testCacheEvictsTheLeastRecentlyUsedTilesDownToTheTarget() {
        let now = Date()
        let files = (0..<10).map { i in
            CachedTileFile(url: URL(fileURLWithPath: "/tmp/dem/\(i).png"), bytes: 100, date: now.addingTimeInterval(Double(i)))
        }
        XCTAssertTrue(DEMService.filesToEvict(files, capBytes: 1_000, targetBytes: 800).isEmpty, "1 000 bytes fit the cap")
        let evicted = DEMService.filesToEvict(files.shuffled(), capBytes: 900, targetBytes: 600)
        XCTAssertEqual(evicted.map(\.lastPathComponent), ["0.png", "1.png", "2.png", "3.png"])
    }

    func testTrimCacheDeletesOldestFiles() throws {
        let dir = try makeTemporaryDirectory("dem-trim-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let fm = FileManager.default
        for i in 0..<6 {
            let url = dir.appendingPathComponent("11/\(i)/0.png")
            try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data(count: 1_000).write(to: url)
            try fm.setAttributes([.modificationDate: Date(timeIntervalSinceNow: Double(i - 10) * 60)], ofItemAtPath: url.path)
        }
        // 6 000 bytes over a 5 000 cap: down to 4 000 (80 %), the two oldest go.
        XCTAssertEqual(DEMService.trimCache(at: dir, capBytes: 5_000), 2)
        XCTAssertFalse(fm.fileExists(atPath: dir.appendingPathComponent("11/0/0.png").path))
        XCTAssertFalse(fm.fileExists(atPath: dir.appendingPathComponent("11/1/0.png").path))
        XCTAssertTrue(fm.fileExists(atPath: dir.appendingPathComponent("11/5/0.png").path))
        XCTAssertEqual(DEMService.trimCache(at: dir, capBytes: 5_000), 0)
    }

    func testDownloadsRetryTransientFailuresButNotMissingTiles() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [FlakyTileURLProtocol.self]
        let session = URLSession(configuration: config)

        let cache = try makeTemporaryDirectory("dem-retry-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: cache) }
        let service = DEMService(cacheDirectory: cache, session: session, retryDelays: [0.01, 0.01])

        FlakyTileURLProtocol.reset(failures: 2, status: 503)
        let tile = TileID(z: 11, x: 1179, y: 1175)
        let data = await service.tile(tile)
        XCTAssertNotNil(data, "two 503s, then the tile")
        XCTAssertEqual(FlakyTileURLProtocol.requests, 3)
        XCTAssertTrue(FileManager.default.fileExists(atPath: service.cacheURL(for: tile).path))

        FlakyTileURLProtocol.reset(failures: 5, status: 503)
        let gaveUp = await service.tile(TileID(z: 11, x: 1180, y: 1175))
        XCTAssertNil(gaveUp)
        XCTAssertEqual(FlakyTileURLProtocol.requests, 3, "one try and two retries")

        FlakyTileURLProtocol.reset(failures: 1, status: 404)
        let missing = await service.tile(TileID(z: 11, x: 1181, y: 1175))
        XCTAssertNil(missing)
        XCTAssertEqual(FlakyTileURLProtocol.requests, 1, "a 404 is not retried")

        // A cached tile needs no request.
        FlakyTileURLProtocol.reset(failures: 0, status: 200)
        let cached = await service.tiles([tile, tile])
        XCTAssertEqual(cached.count, 1)
        XCTAssertEqual(FlakyTileURLProtocol.requests, 0)
        XCTAssertEqual(DEMService.maxConcurrentDownloads, 6)
    }

    // MARK: - Helpers

    private static let pngBytes = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00])

    private static func freshDefaults() throws -> (UserDefaults, String) {
        let suite = "SearchWiderTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        return (defaults, suite)
    }

    private static func needDEM(_ tiles: [TileID], search: SearchStatus? = nil) -> GeorefResponse {
        GeorefResponse(status: "need_dem", needTiles: tiles, search: search)
    }

    private static func failed(_ message: String) -> GeorefResponse {
        GeorefResponse(status: "failed", message: message)
    }

    private static func serve(_ tiles: [TileID]) -> [TileID: Data] {
        Dictionary(uniqueKeysWithValues: tiles.map { ($0, pngBytes) })
    }

    private static func loopResult(_ response: GeorefResponse, cutoff: TerrainPullLoop.Cutoff? = nil,
                                   tally: TerrainPullLoop.Tally = TerrainPullLoop.Tally()) -> TerrainPullLoop.Result {
        TerrainPullLoop.Result(response: response, end: .finished, cutoff: cutoff, tally: tally)
    }

    private static func jsonObject<T: Encodable>(_ value: T) throws -> [String: Any] {
        let data = try JSONEncoder().encode(value)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private static func decode(_ json: String) throws -> GeorefResponse {
        try JSONDecoder().decode(GeorefResponse.self, from: Data(json.utf8))
    }
}

// MARK: - Fakes

/// A scripted core: `script(request, call index)` answers each call; tiles given and marked missing
/// are recorded.
private final class FakeEngine: GeorefEngine, @unchecked Sendable {
    private let lock = NSLock()
    private let script: (GeorefRequest, Int) -> GeorefResponse
    private var _requests: [GeorefRequest] = []
    private var _added: [TileID] = []
    private var _missing: [TileID] = []

    init(_ script: @escaping (GeorefRequest, Int) -> GeorefResponse) {
        self.script = script
    }

    var requests: [GeorefRequest] { lock.withLock { _requests } }
    var added: [TileID] { lock.withLock { _added } }
    var missing: [TileID] { lock.withLock { _missing } }

    func georeference(_ request: GeorefRequest) throws -> GeorefResponse {
        let index = lock.withLock { () -> Int in
            _requests.append(request)
            return _requests.count - 1
        }
        return script(request, index)
    }

    func addDEMTile(_ t: TileID, png: Data) -> Bool {
        lock.withLock { _added.append(t) }
        return true
    }

    func markDEMTileMissing(_ t: TileID) {
        lock.withLock { _missing.append(t) }
    }

    var progress: Float { 0.5 }

    func cancel() {}
}

/// Serves every tile (a tiny PNG), or none (offline).
private final class FakeTerrain: TerrainTileSource {
    let available: Bool

    init(available: Bool = true) {
        self.available = available
    }

    func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)?) async -> [TileID: Data] {
        progress?(ts.count, ts.count)
        guard available else { return [:] }
        let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00])
        var out: [TileID: Data] = [:]
        for t in ts { out[t] = png }
        return out
    }
}

/// Answers lookups from a table, after `delay`; names in `hang` never answer (until cancelled).
private final class MockGeocoder: PlaceGeocoding, @unchecked Sendable {
    private let lock = NSLock()
    private let results: [String: [GeocodedPlace]]
    private let delay: Double
    private let hang: Set<String>
    private var _queries: [String] = []
    private var inFlight = 0
    private var _maxInFlight = 0

    init(results: [String: [GeocodedPlace]], delay: Double = 0, hang: Set<String> = []) {
        self.results = results
        self.delay = delay
        self.hang = hang
    }

    var queries: [String] { lock.withLock { _queries } }
    var maxInFlight: Int { lock.withLock { _maxInFlight } }

    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace] {
        lock.withLock {
            _queries.append(query)
            inFlight += 1
            _maxInFlight = max(_maxInFlight, inFlight)
        }
        defer { lock.withLock { inFlight -= 1 } }
        if hang.contains(query) {
            try await Task.sleep(nanoseconds: 30_000_000_000)
        } else if delay > 0 {
            try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
        }
        return results[query] ?? []
    }
}

/// Stages and fractions as reported, thread-safe.
private final class StageRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [(GeorefStage, Double)] = []

    func add(_ stage: GeorefStage, _ fraction: Double) {
        lock.withLock { entries.append((stage, fraction)) }
    }

    var stages: [GeorefStage] { lock.withLock { entries.map { $0.0 } } }
    var labels: [String] { lock.withLock { entries.map { $0.0.label } } }
    var lastFraction: Double { lock.withLock { entries.last?.1 ?? 0 } }
    var isMonotonic: Bool {
        lock.withLock { zip(entries, entries.dropFirst()).allSatisfy { $0.1 <= $1.1 } }
    }
}

/// Answers tile requests: `failures` times with `status` (and an empty body), then a PNG.
final class FlakyTileURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var remainingFailures = 0
    private static var failureStatus = 503
    private static var requestCount = 0

    static func reset(failures: Int, status: Int) {
        lock.withLock {
            remainingFailures = failures
            failureStatus = status
            requestCount = 0
        }
    }

    static var requests: Int { lock.withLock { requestCount } }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let status = Self.lock.withLock { () -> Int in
            Self.requestCount += 1
            if Self.remainingFailures > 0 {
                Self.remainingFailures -= 1
                return Self.failureStatus
            }
            return 200
        }
        let body = status == 200 ? Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00]) : Data()
        guard let url = request.url,
              let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
