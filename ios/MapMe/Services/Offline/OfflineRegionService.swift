import Foundation
import Observation
import UIKit

/// What downloading an area would take, for the region picker.
struct OfflineEstimate: Equatable, Sendable {
    var plan: TilePlan
    /// Bytes to download: measured from sample tiles when online, else the plan's estimate.
    var bytes: Double
    /// The estimate came from sample tiles' real sizes (else from the planner's table).
    var isMeasured: Bool
    /// Too large to download (`OfflineRegionService.maxTiles`, `.maxBytes`, free space).
    var refusal: String?
    /// Space left on the device when the estimate was made.
    var freeBytes: Int64? = nil
}

/// The network as offline downloads see it (`NetworkPathObserver`, or a stand-in in tests).
struct OfflineNetwork: Equatable, Sendable {
    var online: Bool
    /// Mobile data or a personal hotspot.
    var expensive: Bool
    /// Low Data Mode.
    var constrained: Bool

    static let wifi = OfflineNetwork(online: true, expensive: false, constrained: false)

    static var current: OfflineNetwork {
        let s = NetworkPathObserver.shared.snapshot
        return OfflineNetwork(online: s.online, expensive: s.expensive, constrained: s.constrained)
    }
}

/// The terrain kept around one located map (trailhead caching).
struct TrailheadTerrain: Codable, Hashable, Identifiable, Sendable {
    enum Status: String, Codable, Hashable, Sendable {
        /// Waiting to download (network, Low Data Mode, the battery saver, another map first).
        case pending
        case downloading
        /// Every tile is on the device (`tilesFailed` may count a few that could not be had).
        case complete
        /// The core could not plan it (`message` says why).
        case failed
    }

    var mapID: UUID
    var status: Status = .pending
    var tilesTotal = 0
    var tilesDone = 0
    var tilesFailed = 0
    var bytesOnDisk: Int64 = 0
    var updatedAt = Date()
    var completedAt: Date? = nil
    /// Why it waits or failed ("Waiting for network").
    var message: String? = nil

    var id: UUID { mapID }
    var fraction: Double { tilesTotal > 0 ? min(1, Double(tilesDone) / Double(tilesTotal)) : 0 }
}

/// Offline terrain (docs/design/explorer.md, Offline regions and Trailhead caching): regions the
/// user picks and downloads (with progress, pause, resume, cancel, retries; pinned so the cache
/// never evicts them), and the terrain kept around every map located while online. Use from the
/// main thread; downloads run in the background.
///
/// Regions download one at a time, oldest first (`pump`); a region whose download is blocked
/// (offline, mobile data not allowed, Low Data Mode, storage full, MapMe's background time over)
/// pauses with that reason and resumes by itself when it clears; one the user paused stays
/// paused. Trailheads download one map at a time beside it, at utility priority, over mobile data
/// too (3–14 MB), but not in Low Data Mode or while the battery saver is on.
///
/// Files (Application Support/OfflineRegions, written atomically off the main thread, an
/// unreadable file moved aside, never overwritten):
///
///     regions.json      {"version": 1, "regions": [OfflineRegion]}     newest first
///     trailheads.json   {"version": 1, "maps": [TrailheadTerrain]}
@MainActor
@Observable
final class OfflineRegionService {
    /// Newest first.
    private(set) var regions: [OfflineRegion] = []
    /// The region downloading now.
    private(set) var activeRegionID: UUID?
    /// The terrain kept around located maps, by map.
    private(set) var trailheads: [UUID: TrailheadTerrain] = [:]
    /// Bytes of all pinned terrain on disk (nil until measured).
    private(set) var pinnedBytes: Int64?
    /// How the active download carries on while MapMe is not in front.
    private(set) var backgroundMode: OfflineBackgroundMode = .none

    /// A plan larger than this is refused (about 15 minutes on good Wi-Fi).
    static let maxTiles = OfflinePlanner.maxTiles
    /// An estimate larger than this is refused.
    static let maxBytes = OfflinePlanner.maxBytes
    static let fileVersion = 1
    /// Progress is saved at most this often while downloading (seconds).
    static let saveInterval: TimeInterval = 2
    /// Trailheads download gently beside a region.
    static let trailheadConcurrency = 4

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored let pinned: PinnedTerrainStore
    @ObservationIgnored let directory: URL
    @ObservationIgnored private let downloader: TileDownloader
    @ObservationIgnored private let network: @MainActor () -> OfflineNetwork
    /// The core's tile plan (a stand-in in tests).
    @ObservationIgnored private let planner: @Sendable (TilePlanRequest) -> Result<TilePlan, OfflinePlanFailure>
    @ObservationIgnored private let writer = DispatchQueue(label: "com.jacobusoettle.mapme.offline-regions", qos: .utility)

    @ObservationIgnored private var regionTask: Task<Void, Never>?
    /// Identifies the running region download (a stopped one's late updates are ignored).
    @ObservationIgnored private var regionRun: UUID?
    /// The active download's tiles and how far it got, for the background hand-off.
    @ObservationIgnored private var activeTiles: [TileID] = []
    @ObservationIgnored private var activeCursor = 0
    @ObservationIgnored private var trailheadTask: Task<Void, Never>?
    @ObservationIgnored private var trailheadRun: UUID?
    @ObservationIgnored private(set) var activeTrailheadID: UUID?
    @ObservationIgnored private var conditionsLoop: ObservationLoop?
    @ObservationIgnored private var lastConditions: Conditions?
    @ObservationIgnored private var lastRegionSave = Date.distantPast
    @ObservationIgnored private var lastTrailheadSave = Date.distantPast
    @ObservationIgnored private var pendingRegionSave = false
    @ObservationIgnored private var pendingTrailheadSave = false
    @ObservationIgnored private var backgroundTaskID: UIBackgroundTaskIdentifier = .invalid
    @ObservationIgnored private var lifecycleObservers: [NSObjectProtocol] = []
    @ObservationIgnored private var loaded = false

    /// - Parameters:
    ///   - pinned: where tiles are kept (`.shared`; a mock store with `-mockOfflineDownloads`).
    ///   - directory: where regions.json and trailheads.json live.
    ///   - session: the tile server (the network; a fake in tests).
    ///   - cacheDirectory: the terrain cache whose tiles are moved instead of downloaded.
    ///   - network: the network now.
    ///   - retryDelays: the downloader's pauses before retrying a tile.
    ///   - plan: the tile planner (`OfflinePlanner.plan`, the core).
    init(pinned: PinnedTerrainStore? = nil, directory: URL? = nil, session: URLSession? = nil,
         cacheDirectory: URL? = DEMService.shared.cacheDirectory,
         network: (@MainActor () -> OfflineNetwork)? = nil, retryDelays: [Double]? = nil,
         plan: (@Sendable (TilePlanRequest) -> Result<TilePlan, OfflinePlanFailure>)? = nil) {
        var store = pinned ?? .shared
        var folder = directory ?? URL.applicationSupportDirectory.appendingPathComponent("OfflineRegions", isDirectory: true)
        var tileSession = session
        var cache = cacheDirectory
        var networkNow = network
        if OfflineMocks.mocksDownloads, pinned == nil, directory == nil {
            // UI tests: a fake server and folders of their own, emptied at every launch.
            let mockTerrain = URL.applicationSupportDirectory.appendingPathComponent("Terrain-mock", isDirectory: true)
            folder = URL.applicationSupportDirectory.appendingPathComponent("OfflineRegions-mock", isDirectory: true)
            try? FileManager.default.removeItem(at: mockTerrain)
            try? FileManager.default.removeItem(at: folder)
            store = PinnedTerrainStore(rootURL: mockTerrain)
            tileSession = OfflineMocks.session()
            cache = nil
            // The fake server needs no network: estimates and downloads behave as on Wi-Fi
            // whatever the test machine's connection.
            if networkNow == nil { networkNow = { .wifi } }
        }
        var policy = TileDownloader.Policy()
        if let retryDelays { policy.retryDelays = retryDelays }
        self.pinned = store
        self.directory = folder
        self.downloader = TileDownloader(pinned: store, cacheDirectory: cache, session: tileSession, policy: policy)
        self.network = networkNow ?? { OfflineNetwork.current }
        self.planner = plan ?? { OfflinePlanner.plan($0) }
    }

    func attach(_ model: AppModel) {
        self.model = model
        load()
        BackgroundTileTransfer.shared.use(pinned)
        guard !LocationService.isHostingUnitTests else { return }
        BackgroundTileTransfer.shared.reconnectIfPending()
        OfflineContinuedProcessing.shared.onExpired = { [weak self] in
            self?.continuedProcessingExpired()
        }
        observeLifecycle()
    }

    // MARK: - Queries

    func region(_ id: UUID) -> OfflineRegion? { regions.first { $0.id == id } }

    /// The trailhead terrain of a map, if any.
    func trailhead(for mapID: UUID) -> TrailheadTerrain? { trailheads[mapID] }

    /// Regions with terrain on the device (complete or partly downloaded), newest first.
    var regionsOnDevice: [OfflineRegion] { regions.filter { $0.tilesDone > 0 } }

    /// Maps whose surrounding terrain is (at least partly) on the device.
    var mapsWithTerrain: Int { trailheads.values.filter { $0.tilesDone > 0 }.count }

    // MARK: - Estimates

    /// What downloading `bounds` at `detail` would take (`mm_tile_plan`, then sample sizes).
    func estimate(_ bounds: GeoBounds, detail: OfflineDetail) async -> OfflineEstimate? {
        try? await makeEstimate(bounds, detail: detail).get()
    }

    /// `estimate`, or why the area could not be planned. Online, 16 tiles spread over the plan's
    /// two finest zooms are measured (HEAD requests, ~1 s) and each zoom scaled by them: tiles
    /// weigh 2–4× more over high-resolution sources (US, Europe) than over SRTM, so no table
    /// alone is honest. Offline, the plan's high-resolution figure, marked estimated.
    func makeEstimate(_ bounds: GeoBounds, detail: OfflineDetail) async -> Result<OfflineEstimate, OfflinePlanFailure> {
        let request = OfflinePlanner.regionRequest(bounds, detail: detail)
        let planner = self.planner
        let planned = await Task.detached(priority: .userInitiated) { planner(request) }.value
        let plan: TilePlan
        switch planned {
        case .success(let p): plan = p
        case .failure(let failure): return .failure(failure)
        }
        let free = await Task.detached(priority: .userInitiated) { OfflinePlanner.freeBytes() }.value
        var estimate = OfflineEstimate(plan: plan, bytes: max(plan.bytesHigh, plan.bytes), isMeasured: false, refusal: nil,
                                       freeBytes: free)
        if plan.tiles > 0, plan.tiles <= Self.maxTiles, network().online {
            let samples = OfflinePlanner.sampleTiles(plan)
            let downloader = self.downloader
            let sizes = await Task.detached(priority: .userInitiated) { await downloader.measure(samples) }.value
            if let measured = OfflinePlanner.measuredBytes(plan, samples: sizes) {
                estimate.bytes = measured
                estimate.isMeasured = true
            }
        }
        estimate.refusal = OfflinePlanner.refusal(tiles: plan.tiles, bytes: estimate.bytes, freeBytes: free, detail: detail)
        return .success(estimate)
    }

    // MARK: - Regions

    /// Adds a region and starts (or queues) its download.
    func download(name: String, bounds: GeoBounds, detail: OfflineDetail) {
        download(name: name, bounds: bounds, detail: detail, estimate: nil, allowsCellular: nil)
    }

    /// Adds a region and starts (or queues) its download. `allowsCellular`: the user agreed to
    /// mobile data for it (nil: the setting decides). Returns the region's id.
    @discardableResult
    func download(name: String, bounds: GeoBounds, detail: OfflineDetail, estimate: OfflineEstimate?,
                  allowsCellular: Bool?) -> UUID {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        var region = OfflineRegion(name: trimmed.isEmpty ? "Custom area" : trimmed, bounds: bounds, detail: detail)
        region.tilesTotal = estimate?.plan.tiles ?? 0
        region.estimatedBytes = estimate?.bytes
        region.estimateMeasured = estimate?.isMeasured
        region.allowsCellular = allowsCellular
        regions.insert(region, at: 0)
        saveRegions(force: true)
        beginContinuedProcessing(for: region)
        pump()
        return region.id
    }

    func pause(_ id: UUID) {
        guard let region = self.region(id), region.status == .downloading || region.status == .queued else { return }
        if activeRegionID == id { stopActiveRegion() }
        setPaused(id, .user)
        saveRegions(force: true)
        pump()
    }

    func resume(_ id: UUID) {
        guard let region = self.region(id), region.status != .downloading else { return }
        update(id) {
            $0.status = .queued
            $0.pauseReason = nil
            $0.message = nil
        }
        saveRegions(force: true)
        if let updated = self.region(id) { beginContinuedProcessing(for: updated) }
        pump()
    }

    /// Lets a region waiting for Wi-Fi use mobile data (the user said so).
    func allowCellular(_ id: UUID) {
        update(id) { $0.allowsCellular = true }
        resume(id)
    }

    /// Stops a download and deletes the region and its tiles no one else keeps.
    func delete(_ id: UUID) {
        if activeRegionID == id { stopActiveRegion() }
        regions.removeAll { $0.id == id }
        saveRegions(force: true)
        publishInventory()
        let pinned = self.pinned
        Task { [weak self] in
            await Task.detached(priority: .utility) { pinned.release(.region(id)) }.value
            self?.refreshPinnedBytes()
        }
        pump()
    }

    // MARK: - Trailheads

    /// After a map is located (online): keep the terrain around it (`mm_tile_plan`'s trailhead
    /// rule), owned by the map. Offline, remembered and done when the network is back.
    func cacheTrailhead(for map: MapDocument) {
        guard map.georef != nil else { return }
        if activeTrailheadID == map.id { stopActiveTrailhead() }
        var record = trailheads[map.id] ?? TrailheadTerrain(mapID: map.id)
        record.status = .pending
        record.message = nil
        record.updatedAt = Date()
        trailheads[map.id] = record
        saveTrailheads(force: true)
        pumpTrailheads()
    }

    /// The map was deleted: release its trailhead terrain.
    func releaseTrailhead(mapID: UUID) {
        if activeTrailheadID == mapID { stopActiveTrailhead() }
        trailheads[mapID] = nil
        saveTrailheads(force: true)
        publishInventory()
        let pinned = self.pinned
        Task { [weak self] in
            await Task.detached(priority: .utility) { pinned.release(.map(mapID)) }.value
            self?.refreshPinnedBytes()
            self?.pumpTrailheads()
        }
    }

    /// Settings → "Keep terrain around my maps" turned off: drop all of it (and on: queue every
    /// located map).
    func setKeepsTerrainAroundMaps(_ on: Bool) {
        if on {
            backfillTrailheads()
            pumpTrailheads()
        } else {
            for id in Array(trailheads.keys) { releaseTrailhead(mapID: id) }
        }
    }

    // MARK: - Launch

    /// Once the UI is up: finish interrupted releases, collect background downloads, resume
    /// interrupted regions, queue the terrain around located maps without it.
    func launch() async {
        guard !LocationService.isHostingUnitTests else { return }
        if await BackgroundTileTransfer.shared.cancelPending() > 0 {
            model?.terrain.terrainDidArrive()
        }
        conditionsLoop = ObservationLoop { [weak self] in
            self?.observeConditions()
        }
        await resumeInterrupted()
    }

    /// Finishes interrupted releases and puts regions cut short (killed mid-download, or paused
    /// by something that may have cleared) back in the queue; queues the terrain of located maps
    /// without it; starts what can start.
    func resumeInterrupted() async {
        let pinned = self.pinned
        await Task.detached(priority: .utility) { pinned.finishInterruptedReleases() }.value
        for region in regions {
            // Killed mid-download, or paused by something that may have cleared since.
            if region.status == .downloading || (region.status == .paused && region.pauseReason != .user) {
                update(region.id) {
                    $0.status = .queued
                    $0.pauseReason = nil
                    $0.message = nil
                }
            }
        }
        backfillTrailheads()
        refreshPinnedBytes()
        publishInventory()
        pump()
        pumpTrailheads()
    }

    // MARK: - Region engine

    /// Starts the oldest queued region unless one is downloading; regions blocked by the network
    /// pause with the reason.
    private func pump() {
        guard regionTask == nil else { return }
        for region in regions.reversed() where region.status == .queued {
            if let reason = blocker(for: region) {
                setPaused(region.id, reason)
                continue
            }
            start(region.id)
            return
        }
        saveRegions(force: true)
        // Nothing left to download: end the continued-processing task, also one asked for that
        // iOS has not started (it is then completed as soon as it starts).
        if OfflineContinuedProcessing.shared.isRequested {
            OfflineContinuedProcessing.shared.finish(success: true)
        }
    }

    /// Why `region` cannot download now (nil: it can).
    private func blocker(for region: OfflineRegion) -> OfflinePauseReason? {
        let cellular = region.allowsCellular ?? model?.settings.offlineDownloadsOnCellular ?? false
        return Self.blocker(network: network(), allowsCellular: cellular)
    }

    /// Offline, Low Data Mode, or mobile data without permission (nil: download).
    nonisolated static func blocker(network: OfflineNetwork, allowsCellular: Bool) -> OfflinePauseReason? {
        if !network.online { return .network }
        if network.constrained { return .lowDataMode }
        if network.expensive, !allowsCellular { return .wifi }
        return nil
    }

    private func start(_ id: UUID) {
        guard let region = self.region(id) else { return }
        let run = UUID()
        regionRun = run
        activeRegionID = id
        activeTiles = []
        activeCursor = 0
        update(id) {
            $0.status = .downloading
            $0.pauseReason = nil
            $0.message = nil
        }
        saveRegions(force: true)
        var policy = self.downloader.policy
        policy.allowsExpensive = region.allowsCellular ?? model?.settings.offlineDownloadsOnCellular ?? false
        let downloader = self.downloader.with(policy)
        let pinned = self.pinned
        let planner = self.planner
        let request = OfflinePlanner.regionRequest(region.bounds, detail: region.detail)

        regionTask = Task { [weak self] in
            // The owner's ranges are recorded before any tile, so a crash leaves nothing unowned.
            let planned = await Task.detached(priority: .userInitiated) { () -> Result<[TileID], OfflinePlanFailure> in
                planner(request).map { plan in
                    pinned.setRanges(plan.ranges, for: .region(id))
                    return OfflinePlanner.orderedTiles(plan)
                }
            }.value
            guard let self, self.regionRun == run, !Task.isCancelled else { return }
            let tiles: [TileID]
            switch planned {
            case .failure(let failure):
                self.finishRegion(id, run: run, failure: "Couldn't plan this region: \(failure.message)")
                return
            case .success(let list):
                tiles = list
            }
            self.activeTiles = tiles
            self.update(id) { $0.tilesTotal = tiles.count }
            let inner = Task.detached(priority: .utility) { () -> TileDownloadSummary in
                await downloader.run(tiles) { progress in
                    Task { @MainActor [weak self] in self?.regionProgress(id, run: run, progress) }
                }
            }
            let summary = await withTaskCancellationHandler {
                await inner.value
            } onCancel: {
                inner.cancel()
            }
            self.finishRegion(id, run: run, summary: summary)
        }
    }

    private func regionProgress(_ id: UUID, run: UUID, _ p: TileDownloadProgress) {
        guard regionRun == run else { return }
        activeCursor = p.cursor
        update(id) { r in
            // A resumed run recounts from zero as it skips the tiles already there.
            r.tilesDone = max(r.tilesDone, p.done)
            r.tilesFailed = p.failed
            r.bytesOnDisk = max(r.bytesOnDisk, p.bytes)
            if r.tilesTotal == 0 { r.tilesTotal = p.total }
        }
        saveRegions(force: false)
        if let region = self.region(id) {
            OfflineContinuedProcessing.shared.update(fraction: region.fraction,
                                                     subtitle: "\(region.name) \u{00B7} \(Int((region.fraction * 100).rounded())) %")
        }
    }

    private func finishRegion(_ id: UUID, run: UUID, summary: TileDownloadSummary? = nil, failure: String? = nil) {
        guard regionRun == run else { return }
        regionRun = nil
        regionTask = nil
        activeRegionID = nil
        activeTiles = []
        if let failure {
            update(id) {
                $0.status = .failed
                $0.message = failure
            }
        } else if let summary {
            let p = summary.progress
            if let stop = summary.stop {
                update(id) {
                    $0.tilesDone = max($0.tilesDone, p.done)
                    $0.bytesOnDisk = max($0.bytesOnDisk, p.bytes)
                }
                setPaused(id, stop.pauseReason)
            } else if summary.isComplete {
                update(id) {
                    $0.status = .complete
                    $0.tilesTotal = p.total
                    $0.tilesDone = p.done
                    $0.tilesFailed = p.failed
                    $0.bytesOnDisk = p.bytes
                    $0.completedAt = Date()
                    $0.pauseReason = nil
                    $0.message = p.failed > 0
                        ? "\(OfflineText.tiles(p.failed)) couldn\u{2019}t be downloaded"
                        : nil
                }
            } else {
                // Cut short by something other than a pause or delete (which clear the run
                // first): back in the queue, resuming where it got to.
                update(id) { $0.status = .queued }
            }
        }
        saveRegions(force: true)
        model?.terrain.terrainDidArrive()
        refreshPinnedBytes()
        publishInventory()
        pump()
    }

    /// Cancels the running region download (its state is set by the caller).
    private func stopActiveRegion() {
        regionTask?.cancel()
        regionTask = nil
        regionRun = nil
        activeRegionID = nil
    }

    private func setPaused(_ id: UUID, _ reason: OfflinePauseReason) {
        update(id) {
            $0.status = .paused
            $0.pauseReason = reason
            $0.message = reason.label
        }
    }

    private func update(_ id: UUID, _ change: (inout OfflineRegion) -> Void) {
        guard let i = regions.firstIndex(where: { $0.id == id }) else { return }
        var region = regions[i]
        change(&region)
        if region != regions[i] { regions[i] = region }
    }

    // MARK: - Trailhead engine

    private func pumpTrailheads() {
        guard trailheadTask == nil, let model else { return }
        guard model.settings.cacheTerrainAroundMaps else { return }
        let pending = trailheads.values.filter { $0.status == .pending }.sorted { $0.updatedAt < $1.updatedAt }
        guard let next = pending.first else { return }
        if let reason = trailheadBlocker() {
            for record in pending where record.message != reason {
                trailheads[record.mapID]?.message = reason
            }
            return
        }
        guard let map = model.library.map(id: next.mapID), let request = OfflinePlanner.trailheadRequest(for: map) else {
            // The map is gone or no longer located.
            trailheads[next.mapID] = nil
            saveTrailheads(force: true)
            pumpTrailheads()
            return
        }
        startTrailhead(map.id, request: request)
    }

    /// Why the terrain around maps waits (nil: download now).
    private func trailheadBlocker() -> String? {
        let net = network()
        if !net.online { return "Waiting for network" }
        if net.constrained { return "Waits for Low Data Mode to end" }
        if let battery = model?.battery, !battery.allowsPrefetch { return "Waits for the battery saver to end" }
        return nil
    }

    private func startTrailhead(_ mapID: UUID, request: TilePlanRequest) {
        let run = UUID()
        trailheadRun = run
        activeTrailheadID = mapID
        trailheads[mapID]?.status = .downloading
        trailheads[mapID]?.message = nil
        var policy = self.downloader.policy
        policy.allowsExpensive = true
        policy.concurrency = Self.trailheadConcurrency
        let downloader = self.downloader.with(policy)
        let pinned = self.pinned
        let planner = self.planner
        trailheadTask = Task { [weak self] in
            let planned = await Task.detached(priority: .utility) { () -> Result<[TileID], OfflinePlanFailure> in
                planner(request).map { plan in
                    pinned.setRanges(plan.ranges, for: .map(mapID))
                    return OfflinePlanner.orderedTiles(plan)
                }
            }.value
            guard let self, self.trailheadRun == run, !Task.isCancelled else { return }
            let tiles: [TileID]
            switch planned {
            case .failure(let failure):
                self.finishTrailhead(mapID, run: run, failure: failure.message)
                return
            case .success(let list):
                tiles = list
            }
            self.trailheads[mapID]?.tilesTotal = tiles.count
            let inner = Task.detached(priority: .utility) { () -> TileDownloadSummary in
                await downloader.run(tiles) { progress in
                    Task { @MainActor [weak self] in self?.trailheadProgress(mapID, run: run, progress) }
                }
            }
            let summary = await withTaskCancellationHandler {
                await inner.value
            } onCancel: {
                inner.cancel()
            }
            self.finishTrailhead(mapID, run: run, summary: summary)
        }
    }

    private func trailheadProgress(_ mapID: UUID, run: UUID, _ p: TileDownloadProgress) {
        guard trailheadRun == run, var record = trailheads[mapID] else { return }
        record.tilesDone = max(record.tilesDone, p.done)
        record.tilesFailed = p.failed
        record.bytesOnDisk = max(record.bytesOnDisk, p.bytes)
        if record != trailheads[mapID] { trailheads[mapID] = record }
        saveTrailheads(force: false)
    }

    private func finishTrailhead(_ mapID: UUID, run: UUID, summary: TileDownloadSummary? = nil, failure: String? = nil) {
        guard trailheadRun == run else { return }
        trailheadRun = nil
        trailheadTask = nil
        activeTrailheadID = nil
        if var record = trailheads[mapID] {
            record.updatedAt = Date()
            if let failure {
                record.status = .failed
                record.message = "Couldn\u{2019}t plan the terrain around this map: \(failure)"
            } else if let summary {
                let p = summary.progress
                if let stop = summary.stop {
                    record.status = .pending
                    record.message = stop.pauseReason.label
                } else if summary.isComplete {
                    record.status = .complete
                    record.tilesTotal = p.total
                    record.tilesDone = p.done
                    record.tilesFailed = p.failed
                    record.bytesOnDisk = p.bytes
                    record.completedAt = Date()
                    record.message = p.failed > 0 ? "\(OfflineText.tiles(p.failed)) couldn\u{2019}t be downloaded" : nil
                } else {
                    record.status = .pending
                }
            }
            trailheads[mapID] = record
        }
        saveTrailheads(force: true)
        model?.terrain.terrainDidArrive()
        refreshPinnedBytes()
        publishInventory()
        pumpTrailheads()
    }

    private func stopActiveTrailhead() {
        trailheadTask?.cancel()
        trailheadTask = nil
        trailheadRun = nil
        if let id = activeTrailheadID, trailheads[id]?.status == .downloading {
            trailheads[id]?.status = .pending
        }
        activeTrailheadID = nil
    }

    /// Located maps without terrain kept around them (located before this version, or while the
    /// setting was off) are queued.
    private func backfillTrailheads() {
        guard let model, model.settings.cacheTerrainAroundMaps else { return }
        var changed = false
        for map in model.library.maps where map.georef != nil && trailheads[map.id] == nil {
            trailheads[map.id] = TrailheadTerrain(mapID: map.id)
            changed = true
        }
        if changed { saveTrailheads(force: true) }
    }

    // MARK: - Conditions

    private struct Conditions: Equatable {
        var network: OfflineNetwork
        var allowsPrefetch: Bool
        var keepsTerrain: Bool
        var cellular: Bool
    }

    /// Runs inside an observation: reads the inputs only, reacts on the next turn.
    private func observeConditions() {
        let path = NetworkPathObserver.shared
        let conditions = Conditions(
            network: OfflineNetwork(online: path.isOnline, expensive: path.isExpensive, constrained: path.isConstrained),
            allowsPrefetch: model?.battery.allowsPrefetch ?? true,
            keepsTerrain: model?.settings.cacheTerrainAroundMaps ?? true,
            cellular: model?.settings.offlineDownloadsOnCellular ?? false)
        Task { @MainActor [weak self] in self?.conditionsChanged(conditions) }
    }

    private func conditionsChanged(_ now: Conditions) {
        let before = lastConditions
        lastConditions = now
        guard let before, before != now else { return }
        if now.network.online, !before.network.online {
            // Tiles marked missing while offline may be had now.
            model?.terrain.terrainDidArrive()
        }
        if now.keepsTerrain != before.keepsTerrain {
            setKeepsTerrainAroundMaps(now.keepsTerrain)
        }
        // A running region that may no longer use this network pauses; paused ones whose reason
        // cleared go back in the queue.
        if let id = activeRegionID, let region = self.region(id), let reason = blocker(for: region) {
            stopActiveRegion()
            setPaused(id, reason)
        }
        for region in regions where region.status == .paused {
            guard let reason = region.pauseReason, reason != .user, reason != .background else { continue }
            if blocker(for: region) == nil {
                update(region.id) {
                    $0.status = .queued
                    $0.pauseReason = nil
                    $0.message = nil
                }
            }
        }
        if activeTrailheadID != nil, trailheadBlocker() != nil {
            stopActiveTrailhead()
        }
        saveRegions(force: true)
        pump()
        pumpTrailheads()
    }

    // MARK: - Background

    private func observeLifecycle() {
        let center = NotificationCenter.default
        lifecycleObservers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil,
                                                     queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.didEnterBackground() }
        })
        lifecycleObservers.append(center.addObserver(forName: UIApplication.willEnterForegroundNotification, object: nil,
                                                     queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.willEnterForeground() }
        })
    }

    /// iOS 26 with a continued-processing task: the download runs on. Otherwise the ~30 s iOS
    /// grants, then the next tiles go to the background URLSession and the region pauses.
    private func didEnterBackground() {
        guard regionTask != nil || trailheadTask != nil else { return }
        if OfflineContinuedProcessing.shared.isActive, regionTask != nil {
            backgroundMode = .continuedProcessing
            return
        }
        backgroundMode = .shortGrace
        guard backgroundTaskID == .invalid else { return }
        backgroundTaskID = UIApplication.shared.beginBackgroundTask(withName: "Offline terrain") { [weak self] in
            MainActor.assumeIsolated { self?.backgroundTimeExpired() }
        }
    }

    private func backgroundTimeExpired() {
        if activeTrailheadID != nil { stopActiveTrailhead() }
        if let id = activeRegionID, let region = self.region(id) {
            let remaining = remainingTiles(limit: BackgroundTileTransfer.maxHandoff)
            stopActiveRegion()
            setPaused(id, .background)
            if !remaining.isEmpty {
                let cellular = region.allowsCellular ?? model?.settings.offlineDownloadsOnCellular ?? false
                BackgroundTileTransfer.shared.enqueue(remaining, allowsExpensive: cellular)
                backgroundMode = .handedOff(tiles: remaining.count)
            }
            saveRegions(force: true)
        }
        endBackgroundTask()
    }

    /// The active download's next tiles that are not on the device yet.
    private func remainingTiles(limit: Int) -> [TileID] {
        guard !activeTiles.isEmpty else { return [] }
        var out: [TileID] = []
        var i = max(0, min(activeCursor - TileDownloader.Policy().concurrency, activeTiles.count))
        while i < activeTiles.count, out.count < limit {
            let t = activeTiles[i]
            if !pinned.hasTile(t) { out.append(t) }
            i += 1
        }
        return out
    }

    private func willEnterForeground() {
        endBackgroundTask()
        let handedOff: Bool
        if case .handedOff = backgroundMode { handedOff = true } else { handedOff = false }
        backgroundMode = .none
        Task { [weak self] in
            if handedOff, await BackgroundTileTransfer.shared.cancelPending() > 0 {
                self?.model?.terrain.terrainDidArrive()
            }
            guard let self else { return }
            for region in self.regions where region.status == .paused && region.pauseReason == .background {
                self.update(region.id) {
                    $0.status = .queued
                    $0.pauseReason = nil
                    $0.message = nil
                }
            }
            self.pump()
            self.pumpTrailheads()
        }
    }

    private func endBackgroundTask() {
        guard backgroundTaskID != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTaskID)
        backgroundTaskID = .invalid
    }

    private func beginContinuedProcessing(for region: OfflineRegion) {
        guard !LocationService.isHostingUnitTests else { return }
        OfflineContinuedProcessing.shared.begin(title: "Offline terrain", subtitle: region.name)
    }

    private func continuedProcessingExpired() {
        if UIApplication.shared.applicationState == .background {
            backgroundTimeExpired()
        }
    }

    // MARK: - Persistence

    private struct RegionsFile: Codable {
        var version: Int
        var regions: [OfflineRegion]
    }

    private struct TrailheadsFile: Codable {
        var version: Int
        var maps: [TrailheadTerrain]
    }

    var regionsURL: URL { directory.appendingPathComponent("regions.json") }
    var trailheadsURL: URL { directory.appendingPathComponent("trailheads.json") }

    /// Reads regions.json and trailheads.json (small files) once.
    func load() {
        guard !loaded else { return }
        loaded = true
        if let file: RegionsFile = Self.read(regionsURL) {
            regions = file.regions
        }
        if let file: TrailheadsFile = Self.read(trailheadsURL) {
            trailheads = Dictionary(file.maps.map { ($0.mapID, $0) }, uniquingKeysWith: { a, _ in a })
        }
        publishInventory()
    }

    /// Writes regions.json now (`force`) or at most every `saveInterval` while downloading.
    private func saveRegions(force: Bool) {
        if !force, Date().timeIntervalSince(lastRegionSave) < Self.saveInterval {
            scheduleSave(forTrailheads: false)
            return
        }
        lastRegionSave = Date()
        Self.write(RegionsFile(version: Self.fileVersion, regions: regions), to: regionsURL, on: writer)
    }

    private func saveTrailheads(force: Bool) {
        if !force, Date().timeIntervalSince(lastTrailheadSave) < Self.saveInterval {
            scheduleSave(forTrailheads: true)
            return
        }
        lastTrailheadSave = Date()
        Self.write(TrailheadsFile(version: Self.fileVersion, maps: Array(trailheads.values)), to: trailheadsURL, on: writer)
    }

    private func scheduleSave(forTrailheads: Bool) {
        if forTrailheads ? pendingTrailheadSave : pendingRegionSave { return }
        if forTrailheads { pendingTrailheadSave = true } else { pendingRegionSave = true }
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.saveInterval * 1_000_000_000))
            guard let self else { return }
            if forTrailheads {
                self.pendingTrailheadSave = false
                self.saveTrailheads(force: true)
            } else {
                self.pendingRegionSave = false
                self.saveRegions(force: true)
            }
        }
    }

    /// Waits for the writes queued so far (tests).
    func flush() {
        writer.sync {}
    }

    /// A file's contents; a file that exists but does not decode is moved aside (never overwritten).
    private static func read<T: Decodable>(_ url: URL) -> T? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        if let value = try? JSONDecoder().decode(T.self, from: data) { return value }
        let stamp = Int(Date().timeIntervalSince1970)
        let aside = url.deletingPathExtension().appendingPathExtension("unreadable-\(stamp).json")
        try? FileManager.default.moveItem(at: url, to: aside)
        return nil
    }

    private static func write<T: Encodable & Sendable>(_ value: T, to url: URL, on queue: DispatchQueue) {
        queue.async {
            guard let data = try? JSONEncoder().encode(value) else { return }
            try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: url, options: .atomic)
        }
    }

    // MARK: - Storage and inventory

    /// Measures the pinned terrain on disk (in the background).
    func refreshPinnedBytes() {
        let pinned = self.pinned
        Task { [weak self] in
            let bytes = await Task.detached(priority: .utility) { pinned.totalBytes() }.value
            self?.pinnedBytes = bytes
        }
    }

    private func publishInventory() {
        OfflineTerrainInventory.shared.update(regionNames: regionsOnDevice.map(\.name), mapsWithTerrain: mapsWithTerrain)
    }
}

/// What offline terrain is on the device, for screens that do not hold the app model (the
/// "Search wider" sheet's offline note). Thread-safe snapshot, kept by `OfflineRegionService`.
final class OfflineTerrainInventory: @unchecked Sendable {
    static let shared = OfflineTerrainInventory()

    private let lock = NSLock()
    private var regionNames: [String] = []
    private var mapCount = 0

    func update(regionNames: [String], mapsWithTerrain: Int) {
        lock.withLock {
            self.regionNames = regionNames
            self.mapCount = mapsWithTerrain
        }
    }

    /// "Offline: searching the terrain on your iPhone (Lesotho, terrain around 3 maps)."
    var searchNote: String {
        let (names, maps) = lock.withLock { (regionNames, mapCount) }
        return Self.searchNote(regionNames: names, mapsWithTerrain: maps)
    }

    static func searchNote(regionNames: [String], mapsWithTerrain: Int) -> String {
        var parts = Array(regionNames.prefix(3))
        if regionNames.count > 3 { parts.append("\(regionNames.count - 3) more regions") }
        if mapsWithTerrain > 0 {
            parts.append(mapsWithTerrain == 1 ? "terrain around 1 map" : "terrain around \(mapsWithTerrain) maps")
        }
        guard !parts.isEmpty else {
            return "Offline, and no terrain is saved on this iPhone: only recently used terrain can be searched. Download a region in Settings \u{2192} Offline terrain before you go."
        }
        return "Offline: searching the terrain on your iPhone (\(parts.joined(separator: ", ")))."
    }
}

@MainActor
enum OfflineFeature {
    static func attach(_ model: AppModel) {
        model.offline.attach(model)
    }

    /// Collects background downloads, resumes interrupted ones and queues the terrain of located
    /// maps without it (the iOS 26 continued-processing task is registered at the first download
    /// the user starts). Debug launch arguments: `-mockOfflineDownloads`,
    /// `-mockRegionPlace "Name=s,w,n,e"`.
    static func appDidLaunch(_ model: AppModel) async {
        // `-mockRegionPlace` (the picker) and `-mockOfflineDownloads` (the fake server, set up in
        // `OfflineRegionService.init`) are read where they apply. `-openOfflineRegions` needs
        // the library to open Settings (a lead file): see the offline UI test, which taps there.
        await model.offline.launch()
    }

    static func mapDidLocate(_ map: MapDocument, model: AppModel) {
        guard model.settings.cacheTerrainAroundMaps else { return }
        model.offline.cacheTrailhead(for: map)
    }

    static func mapDidDelete(_ id: UUID, model: AppModel) {
        model.offline.releaseTrailhead(mapID: id)
    }
}
