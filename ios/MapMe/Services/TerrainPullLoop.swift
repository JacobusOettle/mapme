import Foundation

/// The georeferencing engine as `TerrainPullLoop` and `GeoreferenceService` use it: the core's
/// session (`GeorefSession`), or a fake in tests.
protocol GeorefEngine: AnyObject {
    /// Blocking; run off the main thread.
    func georeference(_ request: GeorefRequest) throws -> GeorefResponse
    @discardableResult func addDEMTile(_ t: TileID, png: Data) -> Bool
    func markDEMTileMissing(_ t: TileID)
    /// 0…1 of the running call; any thread.
    var progress: Float { get }
    /// Stops the running call (or the next one); any thread.
    func cancel()
}

extension GeorefSession: GeorefEngine {}

/// Where terrain tiles come from: `DEMService` (disk cache + network), or a fake in tests.
protocol TerrainTileSource: AnyObject {
    /// PNG bytes of the tiles that could be had; missing ones are absent. `progress(done, total)`
    /// may be called from any thread.
    func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)?) async -> [TileID: Data]
}

/// Answers the core's `need_dem` rounds until it is done (`ok` / `failed`), the work is cancelled,
/// or a safety limit trips. A wide search ("Search wider", up to 2 000 km) is answered chunk by
/// chunk, so there is no fixed number of rounds: the loop goes on while the core makes progress
/// (asks for tiles it has not had, or its `search` status advances) and the total stays under
/// the tile cap.
///
/// * Tile cap: once the next round would take the total past `tileCap`, nothing more is
///   downloaded; every tile asked for is marked missing so the core can finish with what it has
///   (at most `maxFinishingCalls` more calls).
/// * Offline: when the first `maxEmptyRounds` rounds bring no tile at all, the same.
/// * Stalled: the core asking only for tiles it was already given (or told are missing) without
///   its search advancing, more than `maxStalledRounds` times in a row, ends the loop.
struct TerrainPullLoop {
    struct Policy: Equatable {
        /// The most distinct tiles downloaded in one locate.
        var tileCap = 2_500
        var maxStalledRounds = 2
        var maxEmptyRounds = 2
        var maxFinishingCalls = 40
        /// Absolute bound on core calls.
        var maxCalls = 2_000
    }

    /// Why the loop ended.
    enum End: Equatable {
        /// The core answered `ok` or `failed` (or `need_dem` with nothing listed).
        case finished
        case cancelled
        /// The core kept asking for tiles it already had.
        case stalled
        /// `maxCalls`, or `maxFinishingCalls` after a cutoff.
        case callLimit
    }

    /// Why downloads stopped before the core finished.
    enum Cutoff: Equatable {
        case tileCap
        case offline
    }

    struct Tally: Equatable {
        /// Core calls made.
        var calls = 0
        /// Download rounds.
        var rounds = 0
        /// Distinct tiles asked from the tile source.
        var requested = 0
        /// Tiles handed to the core.
        var fetched = 0
        /// Tiles marked missing (unavailable, or skipped after a cutoff).
        var missing = 0
    }

    struct Result {
        /// The last response (nil when the first call was cancelled before it ran).
        var response: GeorefResponse?
        var end: End
        var cutoff: Cutoff?
        var tally: Tally
    }

    var policy = Policy()

    /// - Parameters:
    ///   - call: one core call; its argument counts the calls made so far.
    ///   - fetch: downloads tiles (bounded parallelism, disk cache).
    ///   - add: hands a tile to the core; false when it could not be decoded.
    ///   - markMissing: tells the core a tile is unavailable.
    ///   - isCancelled: polled between steps.
    /// - Throws: whatever `call` throws (an engine error).
    func run(call: (Int) async throws -> GeorefResponse,
             fetch: ([TileID]) async -> [TileID: Data],
             add: (TileID, Data) -> Bool,
             markMissing: (TileID) -> Void,
             isCancelled: () -> Bool) async throws -> Result {
        var tally = Tally()
        var answered = Set<TileID>()
        var lastStatus: SearchStatus?
        var stalled = 0
        var emptyRounds = 0
        var finishingCalls = 0
        var cutoff: Cutoff?
        var response: GeorefResponse?

        while true {
            if isCancelled() { return Result(response: response, end: .cancelled, cutoff: cutoff, tally: tally) }
            if tally.calls >= policy.maxCalls { return Result(response: response, end: .callLimit, cutoff: cutoff, tally: tally) }
            let current = try await call(tally.calls)
            tally.calls += 1
            response = current
            if isCancelled() { return Result(response: response, end: .cancelled, cutoff: cutoff, tally: tally) }
            guard current.needsDEM, let needed = current.needTiles, !needed.isEmpty else {
                return Result(response: current, end: .finished, cutoff: cutoff, tally: tally)
            }

            var seen = Set<TileID>()
            let asked = needed.filter { seen.insert($0).inserted }
            let fresh = asked.filter { !answered.contains($0) }
            let advanced = current.search.map { $0.advanced(since: lastStatus) } ?? false
            if let status = current.search { lastStatus = status }
            stalled = fresh.isEmpty && !advanced ? stalled + 1 : 0
            if stalled > policy.maxStalledRounds {
                return Result(response: current, end: .stalled, cutoff: cutoff, tally: tally)
            }

            // Downloads stopped (cap, offline), or nothing new asked: let the core move on without.
            if cutoff == nil, !fresh.isEmpty, tally.requested + fresh.count > policy.tileCap {
                cutoff = .tileCap
            }
            if cutoff != nil || fresh.isEmpty {
                if cutoff != nil {
                    finishingCalls += 1
                    if finishingCalls > policy.maxFinishingCalls {
                        return Result(response: current, end: .callLimit, cutoff: cutoff, tally: tally)
                    }
                }
                for t in asked {
                    markMissing(t)
                    answered.insert(t)
                }
                tally.missing += fresh.count
                continue
            }

            tally.rounds += 1
            tally.requested += fresh.count
            let data = await fetch(fresh)
            if isCancelled() { return Result(response: response, end: .cancelled, cutoff: cutoff, tally: tally) }
            var got = 0
            for t in fresh {
                if let png = data[t], add(t, png) {
                    got += 1
                } else {
                    markMissing(t)
                }
                answered.insert(t)
            }
            tally.fetched += got
            tally.missing += fresh.count - got
            if got == 0 {
                emptyRounds += 1
                // Offline from the start. Once tiles have come, an empty round is terrain that
                // does not exist (or a blip) and the next chunk is still worth fetching.
                if emptyRounds >= policy.maxEmptyRounds, tally.fetched == 0 { cutoff = .offline }
            } else {
                emptyRounds = 0
            }
        }
    }
}
