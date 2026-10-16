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
///
/// Callers share downloads: a tile another call is already loading is waited for, not loaded
/// twice and not skipped, so every caller asks the store again only once its tiles are there (the
/// neighbouring canvas tiles beyond a sheet, the route card and the navigator, the 3D view and
/// the canvas all need the same DEM tiles at once). A download runs in a task of its own, so a
/// caller that gives up (a closed card, a new tap while planning) neither aborts it for the
/// others nor has its tiles marked missing.
final class TerrainProvider: @unchecked Sendable {
    /// Nil only when the core could not make a store; every query then answers nil.
    let store: TerrainStore?
    private let source: TerrainTileSource
    private let lock = NSLock()
    /// The download each tile being loaded belongs to: asking again for one of them waits for it.
    private var inFlight: [TileID: Task<Void, Never>] = [:]
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
    /// Returns once every tile asked for is in the store or marked missing, including tiles another
    /// call was already loading (waited for, not loaded twice, whether that call was `deviceOnly`
    /// or not). A cancelled caller still waits for the download under way (one round at most) but
    /// never aborts it.
    func load(_ tiles: [TileID], deviceOnly: Bool = false) async {
        guard store != nil, !tiles.isEmpty else { return }
        // Task-locals do not reach a detached task: carry the caller's scope over by hand.
        let onDevice = deviceOnly || TerrainFetchScope.deviceOnly
        let priority = Task.currentPriority
        let downloads: [Task<Void, Never>] = lock.withLock {
            var seen = Set<TileID>()
            var fresh: [TileID] = []
            var waits: [Task<Void, Never>] = []
            for t in tiles where seen.insert(t).inserted {
                if let running = inFlight[t] {
                    waits.append(running)
                } else {
                    fresh.append(t)
                }
            }
            guard !fresh.isEmpty else { return waits }
            let batch = fresh
            // Registered under the lock before it can finish: its own clean-up takes the lock too.
            let download = Task.detached(priority: priority) { [self] in
                await self.fetch(batch, onDevice: onDevice)
            }
            for t in batch { inFlight[t] = download }
            waits.append(download)
            return waits
        }
        for download in downloads {
            await download.value
        }
    }

    /// One download of `load`: adds the tiles `source` has to the store and marks the others
    /// missing, then lets the tiles go (waiters ask the store again after this). Runs detached;
    /// should it ever be cancelled, the tiles it did not get are left unmarked (a cancelled
    /// download says nothing about whether the terrain can be had).
    private func fetch(_ tiles: [TileID], onDevice: Bool) async {
        defer {
            lock.withLock {
                for t in tiles { inFlight[t] = nil }
            }
        }
        guard let store else { return }
        let source = self.source
        let got: [TileID: Data]
        if onDevice {
            got = await TerrainFetchScope.onDevice { await source.tiles(tiles, progress: nil) }
        } else {
            got = await source.tiles(tiles, progress: nil)
        }
        let aborted = Task.isCancelled
        for t in tiles {
            if let png = got[t], store.addTile(t, png: png) { continue }
            if !aborted { store.markMissing(t) }
        }
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

    /// Posted by `terrainDidArrive` (object: the provider) on the calling thread: views drawn
    /// without some terrain (the terrain beyond a sheet, the world view's overlay) draw again.
    static let terrainDidArriveNotification = Notification.Name("MapMeTerrainDidArrive")

    /// Terrain that was missing may be on the device now (a region or a map's trailhead finished,
    /// the network is back, the 3D view's Retry): forget the missing marks so queries ask for
    /// those tiles again, and tell the views drawn without them.
    func terrainDidArrive() {
        store?.clearMissing()
        lock.withLock { _generation += 1 }
        NotificationCenter.default.post(name: Self.terrainDidArriveNotification, object: self)
    }
}

/// Runs `action` on the main actor each time `provider` says terrain arrived
/// (`TerrainProvider.terrainDidArrive`); stops when released.
final class TerrainArrivalObserver {
    private let token: NSObjectProtocol

    init(_ provider: TerrainProvider, action: @escaping @MainActor @Sendable () -> Void) {
        token = NotificationCenter.default.addObserver(forName: TerrainProvider.terrainDidArriveNotification,
                                                       object: provider, queue: .main) { _ in
            Task { @MainActor in action() }
        }
    }

    deinit {
        NotificationCenter.default.removeObserver(token)
    }
}
