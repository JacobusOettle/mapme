// OWNER: pipeline agent. Replace the implementation; keep this public API.
import CoreGraphics
import Foundation

/// Tile pyramid on disk: <dir>/<level>/<col>_<row>.jpg, level 0 = full resolution,
/// each level halves the previous; tiles are `tileSize` px squares (edge tiles smaller).
final class TileStore {
    static let defaultTileSize = 512

    /// Write the pyramid for `image` into `directory`. Returns the number of levels.
    static func buildPyramid(from image: CGImage, into directory: URL, tileSize: Int = defaultTileSize,
                             progress: ((Double) -> Void)? = nil) throws -> Int { 1 }

    let map: MapDocument
    let library: MapLibrary

    init(map: MapDocument, library: MapLibrary) {
        self.map = map
        self.library = library
    }

    /// Pixel size of a pyramid level.
    func levelSize(_ level: Int) -> CGSize { map.pixelSize }

    /// Thread-safe, cached. `dark` returns the night-filtered variant (generated and cached on first use).
    func tile(level: Int, col: Int, row: Int, dark: Bool) -> CGImage? { nil }

    /// Drop in-memory caches (memory warning).
    func purgeMemory() {}
}
