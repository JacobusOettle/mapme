import CoreGraphics
import Foundation
import UIKit

/// Contour images from the core's terrain engine (`mm_terrain_render`, docs/design/explorer.md
/// §5): the tiles of the terrain beyond a sheet (in the map's pixels) and of the world view's
/// overlay (in Web Mercator). Blocking and thread-safe (the store is): call it from CATiledLayer's
/// drawing threads or a background task, never the main thread.
enum TerrainContourRenderer {
    /// One rendered image and the engine's answer.
    struct Result {
        /// Nil when the engine could not draw (failed, not implemented, no memory).
        var image: CGImage?
        var answer: TerrainAnswer<ContourRenderInfo>

        /// Part of the image has elevations (the rest is background).
        var hasTerrain: Bool {
            guard image != nil, let info = answer.value else { return false }
            return (info.missingFraction ?? 0) < 0.999
        }

        /// The engine wants these tiles first (it drew what it could meanwhile).
        var wantsTiles: [TileID] { answer.status == .needDEM ? answer.needTiles : [] }
    }

    /// Draws `request` into a `width` × `height` premultiplied RGBA image.
    static func render(_ request: ContourRenderRequest, width: Int, height: Int, store: TerrainStore) -> Result {
        let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()
        guard width > 0, height > 0, width <= 4096, height <= 4096,
              let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                  space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let data = ctx.data
        else { return Result(image: nil, answer: .failed("No memory for the terrain image")) }
        // Bitmap memory is top row first, as the engine writes it (row 0 = y0 / max_y).
        let buffer = UnsafeMutableRawBufferPointer(start: data, count: ctx.bytesPerRow * height)
        let answer = store.render(request, into: buffer, width: width, height: height, bytesPerRow: ctx.bytesPerRow)
        guard answer.value != nil else { return Result(image: nil, answer: answer) }
        return Result(image: ctx.makeImage(), answer: answer)
    }

    /// A colour as the request's `background`: RGBA 0…255, straight alpha.
    static func rgba(_ color: UIColor) -> [Int] {
        var r: CGFloat = 0
        var g: CGFloat = 0
        var b: CGFloat = 0
        var a: CGFloat = 0
        guard color.getRed(&r, green: &g, blue: &b, alpha: &a) else { return [0, 0, 0, 255] }
        return [r, g, b, a].map { Int((min(max($0, 0), 1) * 255).rounded()) }
    }

    /// The DEM zoom for drawing at `metersPerPoint`: about one DEM cell per point (finer adds
    /// nothing a contour line can show), never coarser than zoom 8, at most 12 — 13 once zoomed
    /// in past 2 m per point (docs/design/explorer.md §5) — and at most `cap` (the battery saver).
    static func demZoom(metersPerPoint: Double, latitude: Double, cap: Int) -> Int {
        let cosLat = max(0.05, cos(latitude * .pi / 180))
        // Ground metres per DEM pixel at zoom 0 (256-pixel tiles).
        let groundAtZoom0 = 2 * Double.pi * WebMercator.radius / 256 * cosLat
        let mpp = metersPerPoint.isFinite ? max(metersPerPoint, 0.01) : 100
        let wanted = Int(ceil(log2(groundAtZoom0 / mpp)))
        let finest = max(8, min(metersPerPoint < 2 ? 13 : 12, cap))
        return min(max(wanted, 8), finest)
    }
}
