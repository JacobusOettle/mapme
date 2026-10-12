import Foundation

/// Terrarium DEM tiles from AWS Open Data with an on-disk cache (Caches/dem/z/x/y.png).
///
/// A wide search ("Search wider", up to 2 000 km) can fetch thousands of tiles, so:
/// * at most `maxConcurrentDownloads` requests run at once;
/// * a transient failure (timeout, dropped connection, 5xx, 429) is retried `retryDelays.count`
///   times with a growing pause; offline and 4xx fail at once (the core is told the tile is missing);
/// * the cache is least-recently-used and capped at `cacheCapBytes`: a cache hit refreshes the
///   file's date, and once enough has been written since the last check the oldest tiles go until
///   the cache is back to `cacheTrimFraction` of the cap.
final class DEMService: TerrainTileSource {
    static let shared = DEMService()

    static let maxConcurrentDownloads = 6
    static let defaultCacheCapBytes: Int64 = 400 * 1_000_000
    /// After a trim the cache holds at most this share of the cap, so trims stay rare.
    static let cacheTrimFraction = 0.8
    /// Bytes written between two looks at the cache size.
    static let trimCheckIntervalBytes: Int64 = 8 * 1_000_000
    static let defaultRetryDelays: [Double] = [0.4, 1.2]
    private static let pngSignature: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]

    let cacheDirectory: URL
    let cacheCapBytes: Int64
    private let session: URLSession
    private let retryDelays: [Double]

    private let lock = NSLock()
    /// Bytes written since the cache size was last checked (nil: never checked in this run).
    private var writtenSinceCheck: Int64?
    private var trimming = false

    init(cacheDirectory: URL? = nil, session: URLSession? = nil, cacheCapBytes: Int64 = DEMService.defaultCacheCapBytes,
         retryDelays: [Double] = DEMService.defaultRetryDelays) {
        self.cacheDirectory = cacheDirectory ?? URL.cachesDirectory.appendingPathComponent("dem", isDirectory: true)
        self.cacheCapBytes = cacheCapBytes
        self.retryDelays = retryDelays
        if let session {
            self.session = session
        } else {
            let config = URLSessionConfiguration.default
            config.timeoutIntervalForRequest = 15
            config.timeoutIntervalForResource = 30
            config.httpMaximumConnectionsPerHost = Self.maxConcurrentDownloads
            config.requestCachePolicy = .reloadIgnoringLocalCacheData
            config.urlCache = nil
            self.session = URLSession(configuration: config)
        }
    }

    static func remoteURL(for t: TileID) -> URL? {
        guard isValid(t) else { return nil }
        return URL(string: "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/\(t.z)/\(t.x)/\(t.y).png")
    }

    func cacheURL(for t: TileID) -> URL {
        cacheDirectory
            .appendingPathComponent("\(t.z)", isDirectory: true)
            .appendingPathComponent("\(t.x)", isDirectory: true)
            .appendingPathComponent("\(t.y).png")
    }

    static func isPNG(_ data: Data) -> Bool {
        data.count > pngSignature.count && data.starts(with: pngSignature)
    }

    static func isValid(_ t: TileID) -> Bool {
        guard (0...20).contains(t.z) else { return false }
        let n = 1 << t.z
        return (0..<n).contains(t.x) && (0..<n).contains(t.y)
    }

    /// PNG bytes of one tile, or nil if unavailable (offline / 404 / retries used up).
    func tile(_ t: TileID) async -> Data? {
        let file = cacheURL(for: t)
        if let cached = try? Data(contentsOf: file), Self.isPNG(cached) {
            // Least recently used goes first when the cache is trimmed.
            try? FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: file.path)
            return cached
        }
        guard let url = Self.remoteURL(for: t) else { return nil }
        var attempt = 0
        while true {
            if Task.isCancelled { return nil }
            let retryable: Bool
            do {
                let (data, response) = try await session.data(from: url)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 200, Self.isPNG(data) {
                    try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
                    if (try? data.write(to: file, options: .atomic)) != nil {
                        noteWritten(Int64(data.count))
                    }
                    return data
                }
                retryable = Self.isRetryable(status: status)
            } catch {
                retryable = Self.isRetryable(error)
            }
            guard retryable, attempt < retryDelays.count else { return nil }
            let delay = max(0, retryDelays[attempt])
            attempt += 1
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
        }
    }

    /// Server answers worth another try.
    static func isRetryable(status: Int) -> Bool {
        [408, 429, 500, 502, 503, 504].contains(status)
    }

    /// Network errors worth another try. Offline (`notConnectedToInternet`), cancellation and
    /// bad URLs are not: the tile is reported missing at once.
    static func isRetryable(_ error: Error) -> Bool {
        guard let error = error as? URLError else { return false }
        switch error.code {
        case .timedOut, .networkConnectionLost, .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed,
             .secureConnectionFailed, .badServerResponse, .resourceUnavailable:
            return true
        default:
            return false
        }
    }

    /// Fetch many tiles concurrently (at most `maxConcurrentDownloads` at once). Missing tiles are
    /// absent from the result.
    func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)? = nil) async -> [TileID: Data] {
        var seen = Set<TileID>()
        let unique = ts.filter { seen.insert($0).inserted }
        let total = unique.count
        guard total > 0 else { return [:] }
        progress?(0, total)
        var result: [TileID: Data] = [:]
        var done = 0
        await withTaskGroup(of: (TileID, Data?).self) { group in
            var pending = unique.makeIterator()
            for _ in 0..<Self.maxConcurrentDownloads {
                guard let t = pending.next() else { break }
                group.addTask {
                    let data = await self.tile(t)
                    return (t, data)
                }
            }
            while let item = await group.next() {
                if let data = item.1 { result[item.0] = data }
                done += 1
                progress?(done, total)
                if Task.isCancelled { continue }
                if let t = pending.next() {
                    group.addTask {
                        let data = await self.tile(t)
                        return (t, data)
                    }
                }
            }
        }
        trimCacheIfNeeded()
        return result
    }

    /// Remove the whole cache (Settings → Clear terrain cache). Blocking.
    func clearCache() {
        try? FileManager.default.removeItem(at: cacheDirectory)
        lock.withLock { writtenSinceCheck = 0 }
    }

    // MARK: - Cache size

    private func noteWritten(_ bytes: Int64) {
        lock.withLock {
            if let w = writtenSinceCheck { writtenSinceCheck = w + bytes }
        }
    }

    /// Trims the cache in the background when it may have outgrown the cap: on the first batch
    /// of a run, then after every `trimCheckIntervalBytes` written.
    private func trimCacheIfNeeded() {
        let due = lock.withLock { () -> Bool in
            guard !trimming else { return false }
            let due = writtenSinceCheck.map { $0 >= Self.trimCheckIntervalBytes } ?? true
            if due {
                trimming = true
                writtenSinceCheck = 0
            }
            return due
        }
        guard due else { return }
        let directory = cacheDirectory
        let cap = cacheCapBytes
        Task.detached(priority: .utility) { [weak self] in
            DEMService.trimCache(at: directory, capBytes: cap)
            self?.trimFinished()
        }
    }

    private func trimFinished() {
        lock.withLock { trimming = false }
    }

    /// Delete the least recently used tiles under `directory` when they take more than `capBytes`,
    /// down to `cacheTrimFraction` of it. Blocking. Returns the number of files deleted.
    @discardableResult
    static func trimCache(at directory: URL, capBytes: Int64) -> Int {
        let keys: [URLResourceKey] = [.isRegularFileKey, .fileSizeKey, .contentModificationDateKey]
        guard let enumerator = FileManager.default.enumerator(at: directory, includingPropertiesForKeys: keys) else { return 0 }
        let keySet = Set(keys)
        var files: [CachedTileFile] = []
        while let item = enumerator.nextObject() {
            guard let url = item as? URL,
                  let values = try? url.resourceValues(forKeys: keySet),
                  values.isRegularFile == true
            else { continue }
            files.append(CachedTileFile(url: url, bytes: Int64(values.fileSize ?? 0),
                                        date: values.contentModificationDate ?? .distantPast))
        }
        let target = Int64(Double(capBytes) * cacheTrimFraction)
        let evict = filesToEvict(files, capBytes: capBytes, targetBytes: target)
        for url in evict {
            try? FileManager.default.removeItem(at: url)
        }
        return evict.count
    }

    /// The oldest files to delete so the rest fit in `targetBytes`, or none while the total is
    /// within `capBytes`.
    static func filesToEvict(_ files: [CachedTileFile], capBytes: Int64, targetBytes: Int64) -> [URL] {
        var total = files.reduce(Int64(0)) { $0 + max(0, $1.bytes) }
        guard total > capBytes else { return [] }
        var evict: [URL] = []
        for file in files.sorted(by: { $0.date < $1.date }) {
            guard total > targetBytes else { break }
            evict.append(file.url)
            total -= max(0, file.bytes)
        }
        return evict
    }
}

/// One file of the terrain cache, for `DEMService.filesToEvict`.
struct CachedTileFile: Equatable {
    var url: URL
    var bytes: Int64
    /// Last use (a cache hit refreshes it).
    var date: Date
}
