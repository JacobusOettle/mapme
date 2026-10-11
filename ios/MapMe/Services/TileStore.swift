import CoreGraphics
import CoreImage
import CoreImage.CIFilterBuiltins
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum TileStoreError: Error, LocalizedError {
    case contextFailed
    case encodeFailed(String)

    var errorDescription: String? {
        switch self {
        case .contextFailed: return "Not enough memory to build the map tiles."
        case .encodeFailed(let s): return "Could not write map tile \(s)."
        }
    }
}

/// Tile pyramid on disk: <dir>/<level>/<col>_<row>.jpg, level 0 = full resolution,
/// each level halves the previous; tiles are `tileSize` px squares (edge tiles smaller).
/// Level `l` pixel (x, y) covers base pixels (x·2^l, y·2^l) exactly; level sizes are ceil(size / 2^l).
final class TileStore {
    static let defaultTileSize = 512
    static let jpegQuality = 0.85

    /// Write the pyramid for `image` into `directory`. Returns the number of levels.
    static func buildPyramid(from image: CGImage, into directory: URL, tileSize: Int = defaultTileSize,
                             progress: ((Double) -> Void)? = nil) throws -> Int {
        precondition(tileSize > 0, "tileSize must be positive")
        let fm = FileManager.default
        try? fm.removeItem(at: directory)
        try fm.createDirectory(at: directory, withIntermediateDirectories: true)

        let levels = levelCount(width: image.width, height: image.height, tileSize: tileSize)
        var total = 0
        for level in 0..<levels {
            let s = levelSize(width: image.width, height: image.height, level: level)
            total += tileCount(s.width, tileSize) * tileCount(s.height, tileSize)
        }
        let counter = ProgressCounter(total: total, progress: progress)

        var current = image
        for level in 0..<levels {
            if level > 0 {
                let s = levelSize(width: image.width, height: image.height, level: level)
                guard let next = halve(current, width: s.width, height: s.height) else { throw TileStoreError.contextFailed }
                current = next
            }
            try writeTiles(of: current, level: level, into: directory, tileSize: tileSize, counter: counter)
        }
        return levels
    }

    /// Pixel size of pyramid level `level` for a `width`×`height` base raster.
    static func levelSize(width: Int, height: Int, level: Int) -> (width: Int, height: Int) {
        let f = 1 << max(0, level)
        return (max(1, (width + f - 1) / f), max(1, (height + f - 1) / f))
    }

    /// Levels needed until one level fits in a single tile.
    static func levelCount(width: Int, height: Int, tileSize: Int) -> Int {
        var n = 1
        var w = max(1, width)
        var h = max(1, height)
        while w > tileSize || h > tileSize {
            w = (w + 1) / 2
            h = (h + 1) / 2
            n += 1
        }
        return n
    }

    static func tileURL(in directory: URL, level: Int, col: Int, row: Int) -> URL {
        directory.appendingPathComponent("\(level)", isDirectory: true).appendingPathComponent("\(col)_\(row).jpg")
    }

    /// Opaque 8-bit sRGB bitmap context (alpha byte skipped).
    static func makeContext(width: Int, height: Int) -> CGContext? {
        guard width > 0, height > 0 else { return nil }
        let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()
        return CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                         space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
    }

    static func jpegData(_ image: CGImage, quality: Double = TileStore.jpegQuality) -> Data? {
        let data = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        let options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        CGImageDestinationAddImage(dest, image, options as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return data as Data
    }

    /// Decoded image at `url` (decoded now, not lazily at first draw).
    static func loadImage(at url: URL) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let options: [CFString: Any] = [kCGImageSourceShouldCacheImmediately: true]
        return CGImageSourceCreateImageAtIndex(source, 0, options as CFDictionary)
    }

    private static func tileCount(_ length: Int, _ tileSize: Int) -> Int { (length + tileSize - 1) / tileSize }

    /// Exactly half scale, anchored top-left, so pixel geometry stays aligned between levels.
    private static func halve(_ image: CGImage, width: Int, height: Int) -> CGImage? {
        guard let ctx = makeContext(width: width, height: height) else { return nil }
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        ctx.interpolationQuality = .high
        let w = CGFloat(image.width) / 2
        let h = CGFloat(image.height) / 2
        ctx.draw(image, in: CGRect(x: 0, y: CGFloat(height) - h, width: w, height: h))
        return ctx.makeImage()
    }

    private static func writeTiles(of image: CGImage, level: Int, into directory: URL, tileSize: Int,
                                   counter: ProgressCounter) throws {
        try FileManager.default.createDirectory(at: directory.appendingPathComponent("\(level)", isDirectory: true),
                                                withIntermediateDirectories: true)
        let cols = tileCount(image.width, tileSize)
        let rows = tileCount(image.height, tileSize)
        let failure = FirstError()
        DispatchQueue.concurrentPerform(iterations: cols * rows) { i in
            let col = i % cols
            let row = i / cols
            let rect = CGRect(x: col * tileSize, y: row * tileSize,
                              width: min(tileSize, image.width - col * tileSize),
                              height: min(tileSize, image.height - row * tileSize))
            let url = tileURL(in: directory, level: level, col: col, row: row)
            guard let tile = image.cropping(to: rect), let data = jpegData(tile) else {
                failure.record(TileStoreError.encodeFailed("\(level)/\(col)_\(row)"))
                return
            }
            do {
                try data.write(to: url, options: .atomic)
                counter.increment()
            } catch {
                failure.record(error)
            }
        }
        if let error = failure.error { throw error }
    }

    // MARK: - Instance (tile access for the map canvas)

    let map: MapDocument
    let library: MapLibrary
    private let lightDirectory: URL
    private let darkDirectory: URL
    private let keyPrefix: String

    /// Shared by every store so several open maps cannot exceed the budget together.
    private static let cache: NSCache<NSString, TileImageBox> = {
        let cache = NSCache<NSString, TileImageBox>()
        cache.totalCostLimit = 120 * 1024 * 1024
        cache.name = "MapMe.tiles"
        return cache
    }()

    private static let ciContext = CIContext(options: [
        .workingColorSpace: NSNull(),
        .outputColorSpace: NSNull(),
        .cacheIntermediates: false,
    ])

    init(map: MapDocument, library: MapLibrary) {
        self.map = map
        self.library = library
        lightDirectory = library.tilesDirectory(for: map, dark: false)
        darkDirectory = library.tilesDirectory(for: map, dark: true)
        keyPrefix = map.id.uuidString
    }

    /// Pixel size of a pyramid level.
    func levelSize(_ level: Int) -> CGSize {
        let s = Self.levelSize(width: map.pixelWidth, height: map.pixelHeight, level: level)
        return CGSize(width: s.width, height: s.height)
    }

    /// Thread-safe, cached. `dark` returns the night-filtered variant (generated and cached on first use).
    func tile(level: Int, col: Int, row: Int, dark: Bool) -> CGImage? {
        guard level >= 0, level < max(1, map.levels), col >= 0, row >= 0 else { return nil }
        let key = "\(keyPrefix)/\(dark ? "d" : "l")/\(level)/\(col)_\(row)" as NSString
        if let hit = Self.cache.object(forKey: key) { return hit.image }
        let loaded: CGImage?
        if dark {
            loaded = darkTile(level: level, col: col, row: row)
        } else {
            loaded = Self.loadImage(at: Self.tileURL(in: lightDirectory, level: level, col: col, row: row))
        }
        guard let image = loaded else { return nil }
        Self.cache.setObject(TileImageBox(image), forKey: key, cost: image.bytesPerRow * image.height)
        return image
    }

    /// Drop in-memory caches (memory warning).
    func purgeMemory() {
        Self.cache.removeAllObjects()
    }

    private func darkTile(level: Int, col: Int, row: Int) -> CGImage? {
        let darkURL = Self.tileURL(in: darkDirectory, level: level, col: col, row: row)
        if FileManager.default.fileExists(atPath: darkURL.path), let stored = Self.loadImage(at: darkURL) {
            return stored
        }
        guard let light = Self.loadImage(at: Self.tileURL(in: lightDirectory, level: level, col: col, row: row)),
              let dark = Self.nightFiltered(light)
        else { return nil }
        if let data = Self.jpegData(dark) {
            try? FileManager.default.createDirectory(at: darkURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: darkURL, options: .atomic)
        }
        return dark
    }

    /// Night version of a paper map: invert, rotate hue 180° (so hues survive the inversion:
    /// brown contours stay brown, blue water stays blue), then lift blacks and cut contrast so
    /// white paper becomes ≈ #101418. Works on gamma-encoded values (colour management off).
    static func nightFiltered(_ image: CGImage) -> CGImage? {
        let input = CIImage(cgImage: image)
        let invert = CIFilter.colorInvert()
        invert.inputImage = input
        let hue = CIFilter.hueAdjust()
        hue.inputImage = invert.outputImage
        hue.angle = Float.pi
        let tone = CIFilter.colorMatrix()
        tone.inputImage = hue.outputImage
        tone.rVector = CIVector(x: 0.86, y: 0, z: 0, w: 0)
        tone.gVector = CIVector(x: 0, y: 0.86, z: 0, w: 0)
        tone.bVector = CIVector(x: 0, y: 0, z: 0.86, w: 0)
        tone.aVector = CIVector(x: 0, y: 0, z: 0, w: 1)
        tone.biasVector = CIVector(x: 0.063, y: 0.078, z: 0.094, w: 0)
        guard let output = tone.outputImage else { return nil }
        return ciContext.createCGImage(output, from: input.extent, format: .RGBA8,
                                       colorSpace: CGColorSpace(name: CGColorSpace.sRGB))
    }
}

private final class TileImageBox {
    let image: CGImage
    init(_ image: CGImage) { self.image = image }
}

/// Thread-safe tile counter for pyramid progress.
private final class ProgressCounter {
    private let lock = NSLock()
    private var done = 0
    private let total: Int
    private let progress: ((Double) -> Void)?

    init(total: Int, progress: ((Double) -> Void)?) {
        self.total = max(1, total)
        self.progress = progress
    }

    func increment() {
        let value = lock.withLock { () -> Int in
            done += 1
            return done
        }
        progress?(Double(value) / Double(total))
    }
}

private final class FirstError {
    private let lock = NSLock()
    private var stored: Error?

    func record(_ error: Error) {
        lock.withLock {
            if stored == nil { stored = error }
        }
    }

    var error: Error? { lock.withLock { stored } }
}
