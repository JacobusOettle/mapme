import Foundation
import Observation

/// What downloading an area would take, for the region picker.
struct OfflineEstimate: Equatable, Sendable {
    var plan: TilePlan
    /// Bytes to download: measured from sample tiles when online, else the plan's estimate.
    var bytes: Double
    /// The estimate came from sample tiles' real sizes (else from the planner's table).
    var isMeasured: Bool
    /// Too large to download (`OfflineRegionService.maxTiles`, `.maxBytes`, free space).
    var refusal: String?
}

/// Offline terrain (docs/design/explorer.md, Offline regions and Trailhead caching): regions the
/// user picks and downloads (with progress, pause, resume, cancel, retries; pinned so the cache
/// never evicts them), and the terrain kept around every map located while online. Use from the
/// main thread; downloads run in the background.
@MainActor
@Observable
final class OfflineRegionService {
    /// Newest first.
    private(set) var regions: [OfflineRegion] = []
    /// The region downloading now.
    private(set) var activeRegionID: UUID?

    /// A plan larger than this is refused (about 15 minutes on good Wi-Fi).
    static let maxTiles = 60_000
    /// An estimate larger than this is refused.
    static let maxBytes = 2.5e9

    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// What downloading `bounds` at `detail` would take (`mm_tile_plan`, then sample sizes).
    func estimate(_ bounds: GeoBounds, detail: OfflineDetail) async -> OfflineEstimate? {
        // Filled in by the offline feature.
        nil
    }

    /// Adds a region and starts (or queues) its download.
    func download(name: String, bounds: GeoBounds, detail: OfflineDetail) {
        // Filled in by the offline feature.
    }

    func pause(_ id: UUID) {}
    func resume(_ id: UUID) {}
    /// Stops a download and deletes the region and its tiles no one else keeps.
    func delete(_ id: UUID) {}

    /// After a map is located (online): keep the terrain around it (`mm_tile_plan`'s trailhead
    /// rule), owned by the map. Offline, remembered and done when the network is back.
    func cacheTrailhead(for map: MapDocument) {
        // Filled in by the offline feature.
    }

    /// The map was deleted: release its trailhead terrain.
    func releaseTrailhead(mapID: UUID) {
        // Filled in by the offline feature.
    }
}

@MainActor
enum OfflineFeature {
    static func attach(_ model: AppModel) {
        model.offline.attach(model)
    }

    /// Resumes interrupted downloads, registers the iOS 26 continued-processing task; Debug launch
    /// arguments: `-openOfflineRegions`, `-mockRegionPlace "Name=s,w,n,e"`.
    static func appDidLaunch(_ model: AppModel) async {}

    static func mapDidLocate(_ map: MapDocument, model: AppModel) {
        guard model.settings.cacheTerrainAroundMaps else { return }
        model.offline.cacheTrailhead(for: map)
    }

    static func mapDidDelete(_ id: UUID, model: AppModel) {
        model.offline.releaseTrailhead(mapID: id)
    }
}
