import CoreGraphics
import Foundation

/// Reads the contour-line colour under a tap from the full-resolution (level 0) tiles.
/// Fingers are wider than contour lines, so it looks at a small window around the tap and
/// picks the colour that stands out most from the paper/background there.
enum ContourColorSampler {
    /// RGB 0–255 of the line under `pixel` (base-image pixels), or nil if the tile is missing.
    /// Blocking (may decode a tile); call off the main thread.
    static func sample(store: TileStore, pixel: CGPoint, tileSize: Int, radius: Int = 6) -> [Int]? {
        guard tileSize > 0, pixel.x.isFinite, pixel.y.isFinite, pixel.x >= 0, pixel.y >= 0 else { return nil }
        let x = Int(pixel.x.rounded(.down))
        let y = Int(pixel.y.rounded(.down))
        let col = x / tileSize
        let row = y / tileSize
        guard let tile = store.tile(level: 0, col: col, row: row, dark: false) else { return nil }
        let lx = x - col * tileSize
        let ly = y - row * tileSize
        guard lx < tile.width, ly < tile.height else { return nil }

        let x0 = max(0, lx - radius), y0 = max(0, ly - radius)
        let x1 = min(tile.width - 1, lx + radius), y1 = min(tile.height - 1, ly + radius)
        let w = x1 - x0 + 1, h = y1 - y0 + 1
        guard let px = rgba(of: tile, x: x0, y: y0, width: w, height: h) else { return nil }
        return pick(px, width: w, height: h, centerX: lx - x0, centerY: ly - y0)
    }

    /// RGBA8 pixels of a window of `image` (top-left origin), rows top to bottom.
    /// Draws the image into a window-sized bitmap context offset so only the window lands in it.
    static func rgba(of image: CGImage, x: Int, y: Int, width: Int, height: Int) -> [UInt8]? {
        guard width > 0, height > 0 else { return nil }
        var buffer = [UInt8](repeating: 0, count: width * height * 4)
        let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()
        let drawn: Bool = buffer.withUnsafeMutableBytes { raw -> Bool in
            guard let ctx = CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8,
                                      bytesPerRow: width * 4, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.interpolationQuality = .none
            // CG's origin is bottom-left: image row y must end up in the context's top row.
            let rect = CGRect(x: CGFloat(-x), y: CGFloat(y + height - image.height),
                              width: CGFloat(image.width), height: CGFloat(image.height))
            ctx.draw(image, in: rect)
            return true
        }
        return drawn ? buffer : nil
    }

    /// The line pixel nearest the tap: background = median colour of the brighter half of the
    /// window (paper is lighter than ink); among pixels that stand out from it, proximity wins
    /// over contrast (capped, so black text or blue rivers next to a faint tan contour lose).
    /// The pick is averaged with its near-identical neighbours to suppress JPEG noise.
    /// Tuned on the Grootkloof sheet: ≈96% tan picks for taps up to 4 px off the line.
    static func pick(_ px: [UInt8], width: Int, height: Int, centerX: Int, centerY: Int) -> [Int] {
        let n = width * height
        func channel(_ i: Int, _ c: Int) -> Double { Double(px[i * 4 + c]) }
        func luminance(_ i: Int) -> Double { 0.299 * channel(i, 0) + 0.587 * channel(i, 1) + 0.114 * channel(i, 2) }

        let brighter = (0..<n).sorted { luminance($0) < luminance($1) }.suffix(max(1, n - n / 2))
        var background = [0.0, 0.0, 0.0]
        for c in 0..<3 {
            let values = brighter.map { channel($0, c) }.sorted()
            background[c] = values[values.count / 2]
        }

        var best = -Double.infinity
        var bestIndex = min(n - 1, max(0, centerY * width + centerX))
        for i in 0..<n {
            let dr = channel(i, 0) - background[0], dg = channel(i, 1) - background[1], db = channel(i, 2) - background[2]
            let contrast = (dr * dr + dg * dg + db * db).squareRoot()
            guard contrast >= 25 else { continue }
            let dx = Double(i % width - centerX), dy = Double(i / width - centerY)
            let score = min(contrast, 60) - 4 * (dx * dx + dy * dy).squareRoot()
            if score > best {
                best = score
                bestIndex = i
            }
        }

        let ref = (channel(bestIndex, 0), channel(bestIndex, 1), channel(bestIndex, 2))
        var sum = (0.0, 0.0, 0.0)
        var count = 0.0
        for i in 0..<n {
            let dr = channel(i, 0) - ref.0, dg = channel(i, 1) - ref.1, db = channel(i, 2) - ref.2
            guard dr * dr + dg * dg + db * db <= 24 * 24 else { continue }
            sum.0 += channel(i, 0)
            sum.1 += channel(i, 1)
            sum.2 += channel(i, 2)
            count += 1
        }
        guard count > 0 else { return [Int(ref.0), Int(ref.1), Int(ref.2)] }
        return [Int((sum.0 / count).rounded()), Int((sum.1 / count).rounded()), Int((sum.2 / count).rounded())]
    }
}
