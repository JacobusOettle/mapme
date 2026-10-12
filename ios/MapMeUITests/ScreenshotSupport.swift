import UIKit
import XCTest

/// Median brightness (0…1, Rec. 709 luma of the sRGB values) of the map band of a screenshot: the
/// middle of the screen between the top bar and the bottom panel, left of the control stack. Tells
/// a dark map (paper ≈ 0.13) from a paper one (≈ 0.9) whatever overlays are drawn on it.
@MainActor
func mapBandBrightness(_ screenshot: XCUIScreenshot) -> Double? {
    guard let image = screenshot.image.cgImage else { return nil }
    let w = 48, h = 96
    var pixels = [UInt8](repeating: 0, count: w * h * 4)
    let drawn = pixels.withUnsafeMutableBytes { raw -> Bool in
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                  space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return false }
        ctx.interpolationQuality = .medium
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return true
    }
    guard drawn else { return nil }
    var values: [Double] = []
    // Bitmap rows are stored top row first.
    for y in (h * 25 / 100)..<(h * 55 / 100) {
        for x in (w * 8 / 100)..<(w * 72 / 100) {
            let i = (y * w + x) * 4
            let r = 0.2126 * Double(pixels[i])
            let g = 0.7152 * Double(pixels[i + 1])
            let b = 0.0722 * Double(pixels[i + 2])
            values.append((r + g + b) / 255)
        }
    }
    guard !values.isEmpty else { return nil }
    values.sort()
    return values[values.count / 2]
}
