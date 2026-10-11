import Foundation

/// Terrarium DEM tiles from AWS Open Data with an on-disk cache (Caches/dem/z/x/y.png).
final class DEMService {
    static let shared = DEMService()

    static let maxConcurrentDownloads = 8
    private static let pngSignature: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]

    let cacheDirectory: URL
    private let session: URLSession

    init(cacheDirectory: URL? = nil, session: URLSession? = nil) {
        self.cacheDirectory = cacheDirectory ?? URL.cachesDirectory.appendingPathComponent("dem", isDirectory: true)
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

    /// PNG bytes of one tile, or nil if unavailable (offline / 404).
    func tile(_ t: TileID) async -> Data? {
        let file = cacheURL(for: t)
        if let cached = try? Data(contentsOf: file), Self.isPNG(cached) {
            return cached
        }
        guard let url = Self.remoteURL(for: t) else { return nil }
        do {
            let (data, response) = try await session.data(from: url)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200, Self.isPNG(data) else { return nil }
            try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: file, options: .atomic)
            return data
        } catch {
            return nil
        }
    }

    /// Fetch many tiles concurrently (bounded parallelism). Missing tiles are absent from the result.
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
                if let t = pending.next() {
                    group.addTask {
                        let data = await self.tile(t)
                        return (t, data)
                    }
                }
            }
        }
        return result
    }
}
