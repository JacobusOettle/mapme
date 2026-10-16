import Foundation

/// Terrain tiles kept on the device for good: offline regions and the terrain around located
/// maps (docs/design/explorer.md, Offline regions). Unlike the terrain cache (Caches/dem, least
/// recently used, 400 MB, which iOS may purge), pinned tiles live in Application Support/Terrain
/// (excluded from backups) and stay while any owner's tile ranges cover them:
///
///     Terrain/tiles/{z}/{x}/{y}.png
///     Terrain/owners/{region|map}-<uuid>.json   {"version": 1, "ranges": [TileRange], ...}
///
/// `DEMService` reads pinned tiles before its cache and the network, so matching, profiles, the
/// terrain beyond a sheet and the 3D view work offline from them. Thread-safe.
final class PinnedTerrainStore: @unchecked Sendable {
    static let shared = PinnedTerrainStore()

    let rootURL: URL

    /// Application Support/Terrain.
    static var defaultRootURL: URL {
        URL.applicationSupportDirectory.appendingPathComponent("Terrain", isDirectory: true)
    }

    init(rootURL: URL? = nil) {
        self.rootURL = rootURL ?? Self.defaultRootURL
    }

    var tilesDirectory: URL { rootURL.appendingPathComponent("tiles", isDirectory: true) }
    var ownersDirectory: URL { rootURL.appendingPathComponent("owners", isDirectory: true) }

    func tileURL(for t: TileID) -> URL {
        tilesDirectory
            .appendingPathComponent("\(t.z)", isDirectory: true)
            .appendingPathComponent("\(t.x)", isDirectory: true)
            .appendingPathComponent("\(t.y).png")
    }

    /// The pinned file of a tile, if it is on the device.
    func pinnedTile(_ t: TileID) -> URL? {
        // Filled in by the offline feature (with an in-memory index of the owners' ranges).
        nil
    }

    /// Records `ranges` as `owner`'s (replacing what it had); tiles still need downloading.
    func setRanges(_ ranges: [TileRange], for owner: TerrainOwner) {
        // Filled in by the offline feature.
    }

    /// Drops `owner` and deletes the tiles no other owner covers. Blocking: run off the main thread.
    func release(_ owner: TerrainOwner) {
        // Filled in by the offline feature.
    }

    /// Bytes on disk of all pinned tiles. Blocking.
    func totalBytes() -> Int64 {
        StorageUsage.directorySize(tilesDirectory)
    }
}
