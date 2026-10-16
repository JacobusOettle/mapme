import Foundation

/// Terrain for the explorer features: the core's in-memory `TerrainStore`, fed on demand.
/// A query that lacks tiles answers `.needDEM`; `resolve` loads what it lists (pinned offline
/// regions and trailhead caches first, then the terrain cache, then the network, all through
/// `source`) and asks again. Tiles that cannot be had are marked missing, so queries fall back to
/// coarser zooms or answer with what there is. When terrain arrives later (an offline region or
/// a map's trailhead finished downloading, the network came back) the offline feature calls
/// `terrainDidArrive()`, which forgets the missing marks so the next query asks again.
///
/// Thread-safe. Queries are CPU work in the core: call `resolve` from a background task, and
/// `store` directly from CATiledLayer's drawing threads (then `prefetch` what it asks for and
/// redraw when it arrives). Owned by the offline feature (docs/design/explorer.md, Terrain).
final class TerrainProvider: @unchecked Sendable {
    /// Nil only when the core could not make a store; every query then answers nil.
    let store: TerrainStore?
    private let source: TerrainTileSource
    private let lock = NSLock()
    /// Tiles being loaded now: asking again for one of them does not load it twice.
    private var loading: Set<TileID> = []
    /// Bumped by `terrainDidArrive`: views drawn from partial terrain redraw when it changes.
    private var _generation = 0

    /// Loads at most this many rounds of tiles for one `resolve`.
    static let defaultMaxRounds = 6

    init(store: TerrainStore? = TerrainStore(), source: TerrainTileSource = DEMService.shared) {
        self.store = store
        self.source = source
    }

    /// Changes whenever terrain that was missing may have arrived (`terrainDidArrive`).
    var generation: Int { lock.withLock { _generation } }

    /// Runs `query` on the store until it is complete, loading the tiles it asks for (at most
    /// `maxRounds` times). Returns the last answer: complete, or partial when tiles could not be
    /// had (offline). Nil without a store. `deviceOnly` loads only what is on the device (no
    /// download: the battery saver's "on demand only", a view that must not wait on the network).
    func resolve<Value>(maxRounds: Int = TerrainProvider.defaultMaxRounds, deviceOnly: Bool = false,
                        _ query: (TerrainStore) -> TerrainAnswer<Value>) async -> TerrainAnswer<Value>? {
        guard let store else { return nil }
        var answer = query(store)
        var rounds = 0
        while answer.status == .needDEM, !answer.needTiles.isEmpty, rounds < maxRounds, !Task.isCancelled {
            rounds += 1
            await load(answer.needTiles, deviceOnly: deviceOnly)
            answer = query(store)
        }
        return answer
    }

    /// Loads tiles into the store: the ones `source` has are added, the others marked missing.
    /// Tiles already being loaded by another call are left to it.
    func load(_ tiles: [TileID], deviceOnly: Bool = false) async {
        guard let store else { return }
        let fresh: [TileID] = lock.withLock {
            var seen = Set<TileID>()
            let wanted = tiles.filter { !loading.contains($0) && seen.insert($0).inserted }
            loading.formUnion(wanted)
            return wanted
        }
        guard !fresh.isEmpty else { return }
        let source = self.source
        let got: [TileID: Data]
        if deviceOnly {
            got = await TerrainFetchScope.onDevice { await source.tiles(fresh, progress: nil) }
        } else {
            got = await source.tiles(fresh, progress: nil)
        }
        for t in fresh {
            if let png = got[t], store.addTile(t, png: png) { continue }
            store.markMissing(t)
        }
        lock.withLock { loading.subtract(fresh) }
    }

    /// `load` without waiting, for CATiledLayer's drawing threads; `completion` runs afterwards on
    /// an arbitrary thread (redraw the tiles that asked).
    func prefetch(_ tiles: [TileID], deviceOnly: Bool = false, completion: (@Sendable () -> Void)? = nil) {
        guard !tiles.isEmpty else { return }
        Task.detached(priority: .utility) { [self] in
            await self.load(tiles, deviceOnly: deviceOnly)
            completion?()
        }
    }

    /// Terrain that was missing may be on the device now (a region or a map's trailhead finished,
    /// the network is back): forget the missing marks so queries ask for those tiles again.
    func terrainDidArrive() {
        store?.clearMissing()
        lock.withLock { _generation += 1 }
    }
}
