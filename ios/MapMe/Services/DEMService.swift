// OWNER: pipeline agent. Replace the implementation; keep this public API.
import Foundation

/// Terrarium DEM tiles from AWS Open Data with an on-disk cache (Caches/dem/z/x/y.png).
final class DEMService {
    static let shared = DEMService()

    /// PNG bytes of one tile, or nil if unavailable (offline / 404).
    func tile(_ t: TileID) async -> Data? { nil }

    /// Fetch many tiles concurrently (bounded parallelism). Missing tiles are absent from the result.
    func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)? = nil) async -> [TileID: Data] { [:] }
}
