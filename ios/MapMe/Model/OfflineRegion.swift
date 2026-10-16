import Foundation

// Offline terrain: regions the user downloads, and who keeps each pinned tile on the device
// (docs/design/explorer.md, Offline regions and Trailhead caching).

/// How much terrain a region keeps.
enum OfflineDetail: String, Codable, CaseIterable, Identifiable, Sendable {
    /// Zooms 8–12: locate maps offline (the coarse search and a refinement on ~38 m cells),
    /// elevation profiles, the terrain beyond a sheet, the 3D view.
    case standard
    /// Zooms 8–13: as online (refinement on ~19 m cells), the sharpest profiles and 3D.
    case high

    var id: String { rawValue }

    var zooms: [Int] {
        switch self {
        case .standard: return [8, 9, 10, 11, 12]
        case .high: return [8, 9, 10, 11, 12, 13]
        }
    }

    var label: String {
        switch self {
        case .standard: return "Standard"
        case .high: return "High detail"
        }
    }

    var summary: String {
        switch self {
        case .standard: return "Find and locate maps, elevation profiles, terrain beyond your maps and 3D."
        case .high: return "Everything at full detail: the most accurate locating offline and the sharpest 3D."
        }
    }
}

/// Where a region download stands.
enum OfflineRegionStatus: String, Codable, Hashable, Sendable {
    /// Planned, not started (or waiting for Wi-Fi).
    case queued
    case downloading
    case paused
    /// Every tile is on the device (`tilesFailed` may still list a few that could not be had).
    case complete
    case failed
}

/// A downloaded (or downloading) terrain region.
struct OfflineRegion: Codable, Hashable, Identifiable, Sendable {
    var id: UUID = UUID()
    /// "Lesotho", "Around Giant's Castle".
    var name: String
    var bounds: GeoBounds
    var detail: OfflineDetail
    var createdAt: Date = Date()
    var status: OfflineRegionStatus = .queued
    var tilesTotal: Int = 0
    var tilesDone: Int = 0
    var tilesFailed: Int = 0
    var bytesOnDisk: Int64 = 0
    /// The estimate shown before downloading (bytes).
    var estimatedBytes: Double? = nil
    /// Why it paused or failed, for the list ("Waiting for Wi-Fi", "Storage full").
    var message: String? = nil
    var completedAt: Date? = nil
    // Added with the download engine (Optional: regions saved before decode without them).
    /// Why a paused region is paused; decides whether it resumes by itself.
    var pauseReason: OfflinePauseReason? = nil
    /// The user agreed to download this region over mobile data.
    var allowsCellular: Bool? = nil
    /// `estimatedBytes` was measured from sample tiles (else the planner's table).
    var estimateMeasured: Bool? = nil

    var fraction: Double { tilesTotal > 0 ? min(1, Double(tilesDone) / Double(tilesTotal)) : 0 }
}

/// Why a region download is paused. Every reason but `.user` clears by itself (the network comes
/// back, Wi-Fi, Low Data Mode off, space freed, MapMe in front again) and the download resumes.
enum OfflinePauseReason: String, Codable, Hashable, Sendable {
    /// Paused by the user.
    case user
    /// No network.
    case network
    /// On mobile data, which this region may not use.
    case wifi
    /// Low Data Mode is on.
    case lowDataMode
    /// The device is out of space.
    case storageFull
    /// iOS stopped MapMe's background time; resumes when MapMe is in front.
    case background

    var label: String {
        switch self {
        case .user: return "Paused"
        case .network: return "Waiting for network"
        case .wifi: return "Waiting for Wi-Fi"
        case .lowDataMode: return "Paused in Low Data Mode"
        case .storageFull: return "Paused: storage full"
        case .background: return "Paused in the background \u{00B7} resumes when you open MapMe"
        }
    }
}

/// Who keeps pinned terrain tiles on the device: a tile stays while any owner's ranges cover it.
enum TerrainOwner: Hashable, Codable, Sendable {
    /// An offline region the user downloaded.
    case region(UUID)
    /// The terrain cached around a located map (deleted with the map).
    case map(UUID)

    /// File name stem of the owner's manifest ("region-<uuid>", "map-<uuid>").
    var fileStem: String {
        switch self {
        case .region(let id): return "region-\(id.uuidString)"
        case .map(let id): return "map-\(id.uuidString)"
        }
    }
}
