import CoreGraphics
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
///
/// An instance serves one map's tiles, light or dark (drawn by the core with the map's
/// `DarkStyle`), to CATiledLayer's drawing threads. `@unchecked Sendable`: everything it holds is
/// immutable (the map, file URLs) except the lazily built dark style, which `styleLock` guards;
/// the tile cache is an NSCache (thread-safe).
final class TileStore: @unchecked Sendable {
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
    private let folder: URL
    private let lightDirectory: URL
    private let darkDirectory: URL
    private let legacyDarkDirectory: URL
    private let profileURL: URL
    private let thumbnailURL: URL
    private let darkThumbnailURL: URL
    private let keyPrefix: String

    /// The dark style, built once on first use (`darkStyle`).
    private let styleLock = NSLock()
    private var styleResolved = false
    private var cachedStyle: DarkStyle?

    /// Shared by every store so several open maps cannot exceed the budget together.
    private static let cache: NSCache<NSString, TileImageBox> = {
        let cache = NSCache<NSString, TileImageBox>()
        cache.totalCostLimit = 120 * 1024 * 1024
        cache.name = "MapMe.tiles"
        return cache
    }()

    /// Long side limit of the overview the dark profile is computed from.
    static let darkOverviewLongSide = 2048

    /// Profile used when the core cannot profile a map (too small, blank): white paper everywhere,
    /// print-like ink, no chroma gain.
    static let neutralDarkProfile =
        #"{"status":"ok","version":1,"ink":0.15,"dead":0.03,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1.0,1.0,1.0]}}"#

    /// Paths are worked out here (from the library's root), so the store can be used from any thread.
    init(map: MapDocument, library: MapLibrary) {
        self.map = map
        folder = library.directory(for: map.id)
        lightDirectory = library.tilesDirectory(for: map, dark: false)
        darkDirectory = library.tilesDirectory(for: map, dark: true)
        legacyDarkDirectory = library.legacyDarkTilesDirectory(for: map)
        profileURL = library.darkProfileURL(for: map)
        thumbnailURL = library.thumbnailURL(for: map)
        darkThumbnailURL = library.darkThumbnailURL(for: map)
        keyPrefix = map.id.uuidString
    }

    /// Pixel size of a pyramid level.
    func levelSize(_ level: Int) -> CGSize {
        let s = Self.levelSize(width: map.pixelWidth, height: map.pixelHeight, level: level)
        return CGSize(width: s.width, height: s.height)
    }

    /// Thread-safe, cached. `dark` returns the core's dark rendering of the tile (`DarkStyle`),
    /// made on first use (or by `prerenderDark`) and kept in `tiles-dark-v2`.
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

    // MARK: Dark map

    /// The map's dark style: from `dark-profile.json`, or profiled now from an overview of the map
    /// (and saved) when that is missing or unusable. Thread-safe; callers arriving while it is
    /// being built wait for it. Nil only when the core rejects even `neutralDarkProfile`.
    var darkStyle: DarkStyle? {
        styleLock.lock()
        defer { styleLock.unlock() }
        if !styleResolved {
            cachedStyle = makeDarkStyle()
            styleResolved = true
        }
        return cachedStyle
    }

    private func makeDarkStyle() -> DarkStyle? {
        let fm = FileManager.default
        if fm.fileExists(atPath: legacyDarkDirectory.path) {
            try? fm.removeItem(at: legacyDarkDirectory)
        }
        if let data = try? Data(contentsOf: profileURL), let stored = DarkStyle(profileJSON: String(decoding: data, as: UTF8.self)) {
            return stored
        }
        if let json = computeDarkProfile(), let fresh = DarkStyle(profileJSON: json) {
            // Data.write never creates folders, so a map deleted meanwhile stays deleted.
            try? Data(json.utf8).write(to: profileURL, options: .atomic)
            return fresh
        }
        return DarkStyle(profileJSON: Self.neutralDarkProfile)
    }

    /// The map's dark profile from an overview: the largest pyramid level whose long side is at
    /// most `darkOverviewLongSide` (the full image when it is that small), stitched from its tiles.
    private func computeDarkProfile() -> String? {
        let level = Self.overviewLevel(width: map.pixelWidth, height: map.pixelHeight, levels: max(1, map.levels))
        guard let ctx = stitchedLevel(level), let data = ctx.data else { return nil }
        let pixels = UnsafeRawBufferPointer(start: data, count: ctx.bytesPerRow * ctx.height)
        return MapMeCore.darkProfile(rgba: pixels, width: ctx.width, height: ctx.height, bytesPerRow: ctx.bytesPerRow)
    }

    /// Finest pyramid level whose long side is at most `maxLongSide` (the coarsest level if none is).
    static func overviewLevel(width: Int, height: Int, levels: Int, maxLongSide: Int = darkOverviewLongSide) -> Int {
        var level = 0
        while level < levels - 1 {
            let s = levelSize(width: width, height: height, level: level)
            if max(s.width, s.height) <= maxLongSide { break }
            level += 1
        }
        return level
    }

    /// All of pyramid level `level`, stitched from its light tiles into one opaque RGBA8 sRGB
    /// context (missing tiles stay white).
    func stitchedLevel(_ level: Int) -> CGContext? {
        let size = Self.levelSize(width: map.pixelWidth, height: map.pixelHeight, level: level)
        guard let ctx = Self.makeContext(width: size.width, height: size.height) else { return nil }
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: size.width, height: size.height))
        ctx.interpolationQuality = .none
        let t = max(1, map.tileSize)
        for row in 0..<Self.tileCount(size.height, t) {
            for col in 0..<Self.tileCount(size.width, t) {
                guard let tile = Self.loadImage(at: Self.tileURL(in: lightDirectory, level: level, col: col, row: row)) else {
                    continue
                }
                // Bitmap contexts have their origin at the bottom left.
                let y = size.height - row * t - tile.height
                ctx.draw(tile, in: CGRect(x: col * t, y: y, width: tile.width, height: tile.height))
            }
        }
        return ctx
    }

    /// The part of the map a tile covers, as fractions of the full map's width and height
    /// (`mm_dark_render`'s rectangle): level `level` pixels are 2^level base pixels wide.
    func normalizedRect(level: Int, col: Int, row: Int, tileWidth: Int, tileHeight: Int)
        -> (u0: Double, v0: Double, du: Double, dv: Double) {
        let scale = Double(1 << max(0, level))
        let t = Double(max(1, map.tileSize))
        let w = Double(max(1, map.pixelWidth))
        let h = Double(max(1, map.pixelHeight))
        return (u0: Double(col) * t * scale / w, v0: Double(row) * t * scale / h,
                du: Double(tileWidth) * scale / w, dv: Double(tileHeight) * scale / h)
    }

    /// `image` drawn dark with `style` as the part [u0, u0 + du] × [v0, v0 + dv] of the map.
    static func darkImage(_ image: CGImage, style: DarkStyle, u0: Double, v0: Double, du: Double, dv: Double) -> CGImage? {
        guard let ctx = makeContext(width: image.width, height: image.height), let data = ctx.data else { return nil }
        ctx.interpolationQuality = .none
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        let pixels = UnsafeMutableRawBufferPointer(start: data, count: ctx.bytesPerRow * ctx.height)
        guard style.render(pixels, width: ctx.width, height: ctx.height, bytesPerRow: ctx.bytesPerRow,
                           u0: u0, v0: v0, du: du, dv: dv)
        else { return nil }
        return ctx.makeImage()
    }

    /// Dark tile from `tiles-dark-v2`, else rendered from the light tile and stored there. Falls
    /// back to the light tile if the core cannot render (the map stays usable).
    private func darkTile(level: Int, col: Int, row: Int) -> CGImage? {
        let darkURL = Self.tileURL(in: darkDirectory, level: level, col: col, row: row)
        if FileManager.default.fileExists(atPath: darkURL.path), let stored = Self.loadImage(at: darkURL) {
            return stored
        }
        guard let light = Self.loadImage(at: Self.tileURL(in: lightDirectory, level: level, col: col, row: row)) else {
            return nil
        }
        let r = normalizedRect(level: level, col: col, row: row, tileWidth: light.width, tileHeight: light.height)
        guard let style = darkStyle,
              let dark = Self.darkImage(light, style: style, u0: r.u0, v0: r.v0, du: r.du, dv: r.dv)
        else { return light }
        storeDarkTile(dark, at: darkURL)
        return dark
    }

    /// Writes a dark tile, creating `tiles-dark-v2/<level>` inside the map folder but never the
    /// map folder itself (a map deleted meanwhile stays deleted).
    private func storeDarkTile(_ image: CGImage, at url: URL) {
        let fm = FileManager.default
        let levelDirectory = url.deletingLastPathComponent()
        if !fm.fileExists(atPath: levelDirectory.path) {
            try? fm.createDirectory(at: darkDirectory, withIntermediateDirectories: false)
            try? fm.createDirectory(at: levelDirectory, withIntermediateDirectories: false)
        }
        guard let data = Self.jpegData(image) else { return }
        try? data.write(to: url, options: .atomic)
    }

    /// Draws `thumb.jpg` dark into `thumb-dark.jpg` (the library card in dark appearance).
    /// False when there is no thumbnail to draw from or the file cannot be written. Blocking.
    @discardableResult
    func writeDarkThumbnail() -> Bool {
        guard let thumb = Self.loadImage(at: thumbnailURL), let style = darkStyle,
              let dark = Self.darkImage(thumb, style: style, u0: 0, v0: 0, du: 1, dv: 1),
              let data = Self.jpegData(dark, quality: 0.8)
        else { return false }
        do {
            try data.write(to: darkThumbnailURL, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    /// Renders the dark thumbnail (again: it is cheap and the light one may be new) and every dark
    /// tile not on disk yet, so the first dark view is instant: the coarse levels first (a handful
    /// of tiles that cover the whole map when zoomed out), then the detail levels. Stops when
    /// `shouldStop` says so or the map's folder is gone (deleted). Blocking: run at utility priority.
    func prerenderDark(shouldStop: () -> Bool = { Task.isCancelled }) {
        let fm = FileManager.default
        guard !shouldStop(), fm.fileExists(atPath: folder.path) else { return }
        writeDarkThumbnail()
        let t = max(1, map.tileSize)
        for level in stride(from: max(1, map.levels) - 1, through: 0, by: -1) {
            let size = Self.levelSize(width: map.pixelWidth, height: map.pixelHeight, level: level)
            for row in 0..<Self.tileCount(size.height, t) {
                for col in 0..<Self.tileCount(size.width, t) {
                    guard !shouldStop(), fm.fileExists(atPath: folder.path) else { return }
                    let url = Self.tileURL(in: darkDirectory, level: level, col: col, row: row)
                    if fm.fileExists(atPath: url.path) { continue }
                    autoreleasepool {
                        _ = darkTile(level: level, col: col, row: row)
                    }
                }
            }
        }
    }
}

/// Background rendering of a map's dark tiles and dark thumbnail after an import or a new
/// flattening (utility priority), so the first dark view is instant. One job per map: starting
/// again replaces the running job; deleting the map cancels it.
enum DarkPrerender {
    private static let lock = NSLock()
    private static var jobs: [UUID: (token: UUID, task: Task<Void, Never>)] = [:]

    static func start(_ map: MapDocument, library: MapLibrary) {
        let store = TileStore(map: map, library: library)
        let id = map.id
        let token = UUID()
        lock.lock()
        defer { lock.unlock() }
        jobs[id]?.task.cancel()
        // The job can only unregister itself once this lock is released, i.e. after it is registered.
        let task = Task.detached(priority: .utility) {
            store.prerenderDark()
            DarkPrerender.finished(id, token: token)
        }
        jobs[id] = (token: token, task: task)
    }

    /// Is a job running for this map?
    static func isRunning(_ id: UUID) -> Bool {
        lock.withLock { jobs[id] != nil }
    }

    /// Cancels the job for map `id`, if any; `whenStopped` runs (off the main thread) once it has
    /// stopped, which may take until the tile it is on is written.
    static func cancel(_ id: UUID, whenStopped: (() -> Void)? = nil) {
        let job = lock.withLock { jobs.removeValue(forKey: id) }
        guard let job else { return }
        job.task.cancel()
        guard let whenStopped else { return }
        Task.detached(priority: .utility) {
            await job.task.value
            whenStopped()
        }
    }

    /// Cancels the job for map `id`, if any, and waits until it has stopped.
    static func cancelAndWait(_ id: UUID) async {
        let job = lock.withLock { jobs.removeValue(forKey: id) }
        if let job {
            job.task.cancel()
            await job.task.value
        }
    }

    private static func finished(_ id: UUID, token: UUID) {
        lock.withLock {
            if jobs[id]?.token == token { jobs[id] = nil }
        }
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
