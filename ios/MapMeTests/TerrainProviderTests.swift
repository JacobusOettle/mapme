import Foundation
import XCTest
@testable import MapMe

/// `TerrainProvider` with several callers at once: downloads are shared, never skipped, and a
/// caller that gives up does not leave its tiles marked missing (docs/design/explorer.md, Terrain).
final class TerrainProviderTests: XCTestCase {
    /// On the Grootkloof sheet, inside the fixture's z13 terrain (fixtures/grootkloof/dem).
    private let point = LatLon(lat: -25.8460, lon: 27.3870)

    /// Fixture terrain served slowly (like a download over a mountain link), counting each tile
    /// asked for; a fetch whose task was cancelled gets nothing, as from `DEMService`.
    private final class SlowFixtureTiles: TerrainTileSource, @unchecked Sendable {
        private let lock = NSLock()
        private var asked: [TileID: Int] = [:]
        private let delayNanoseconds: UInt64

        init(delaySeconds: Double) {
            delayNanoseconds = UInt64(delaySeconds * 1_000_000_000)
        }

        /// How often each tile was fetched.
        var requests: [TileID: Int] { lock.withLock { asked } }

        func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)?) async -> [TileID: Data] {
            lock.withLock {
                for t in ts { asked[t, default: 0] += 1 }
            }
            try? await Task.sleep(nanoseconds: delayNanoseconds)
            if Task.isCancelled { return [:] }
            var out: [TileID: Data] = [:]
            for t in ts {
                if let png = Fixtures.demTile(t) { out[t] = png }
            }
            return out
        }
    }

    func testCallersWaitForTilesAnotherCallIsLoading() async throws {
        // The route card and the navigator (or two canvas tiles) asking for the same terrain at once.
        let tiles = SlowFixtureTiles(delaySeconds: 0.4)
        let terrain = TerrainProvider(store: try XCTUnwrap(TerrainStore()), source: tiles)
        let request = ElevationsRequest(points: [point], maxZoom: 13)
        async let first = terrain.resolve { $0.elevations(request) }
        async let second = terrain.resolve { $0.elevations(request) }
        let answers = await [first, second]
        for answered in answers {
            let answer = try XCTUnwrap(answered)
            XCTAssertEqual(answer.status, .ok, "both callers get the whole answer, not the partial one")
            XCTAssertEqual(answer.value?.zoom, 13)
            let elevation = try XCTUnwrap(answer.value?.elevationsM.first)
            XCTAssertNotNil(elevation)
        }
        XCTAssertFalse(tiles.requests.isEmpty)
        XCTAssertTrue(tiles.requests.values.allSatisfy { $0 == 1 }, "each tile downloaded once: \(tiles.requests)")
    }

    func testAQueryThatGivesUpLeavesItsTerrainToBeHad() async throws {
        // A route card closed (or a new tap while planning) while its terrain is downloading.
        let tiles = SlowFixtureTiles(delaySeconds: 0.4)
        let terrain = TerrainProvider(store: try XCTUnwrap(TerrainStore()), source: tiles)
        let request = ElevationsRequest(points: [point], maxZoom: 13)
        let gaveUp = Task { await terrain.resolve { $0.elevations(request) } }
        try await Task.sleep(nanoseconds: 100_000_000)
        gaveUp.cancel()
        _ = await gaveUp.value

        let resolved = await terrain.resolve { $0.elevations(request) }
        let answer = try XCTUnwrap(resolved)
        XCTAssertEqual(answer.status, .ok)
        XCTAssertEqual(answer.value?.zoom, 13, "the download went on and nothing was marked missing")
        XCTAssertTrue(tiles.requests.values.allSatisfy { $0 == 1 }, "nothing downloaded twice: \(tiles.requests)")
    }

    func testTerrainThatCannotBeHadIsMarkedMissing() async throws {
        // Offline, nothing on the device: the query still ends, with what there is.
        let tiles = SlowFixtureTiles(delaySeconds: 0)
        let terrain = TerrainProvider(store: try XCTUnwrap(TerrainStore()), source: tiles)
        let far = ElevationsRequest(points: [LatLon(lat: 46.5, lon: 8.0)], maxZoom: 13)
        let resolved = await terrain.resolve { $0.elevations(far) }
        let answer = try XCTUnwrap(resolved)
        XCTAssertEqual(answer.status, .ok, "missing marks let the query finish")
        let elevation = try XCTUnwrap(answer.value?.elevationsM.first)
        XCTAssertNil(elevation)
    }
}
