import Foundation

/// What became of one tile in a download.
enum TileFetchOutcome: Equatable, Sendable {
    /// Downloaded and pinned (bytes).
    case downloaded(Int64)
    /// Already pinned (bytes on disk).
    case present(Int64)
    /// Moved from the terrain cache into the pinned store (bytes).
    case adopted(Int64)
    /// The server has no tile there (404): nothing to keep, counted as done.
    case noData
    /// Retries used up, or the server sent something that is not a tile.
    case failed
    /// The download as a whole cannot go on now.
    case stop(TileDownloadStop)
    case cancelled
}

/// Why a download stopped before its end; the region pauses with the matching reason and resumes
/// when it clears.
enum TileDownloadStop: Equatable, Sendable {
    case offline
    /// On mobile data, which this download may not use.
    case wifi
    case lowDataMode
    case storageFull

    var pauseReason: OfflinePauseReason {
        switch self {
        case .offline: return .network
        case .wifi: return .wifi
        case .lowDataMode: return .lowDataMode
        case .storageFull: return .storageFull
        }
    }
}

/// How far a download has got.
struct TileDownloadProgress: Equatable, Sendable {
    /// Tiles in the download.
    var total: Int
    /// Tiles on the device (downloaded, already there, moved from the cache) or with no data.
    var done = 0
    /// Tiles that could not be had (retries used up).
    var failed = 0
    /// Bytes on disk of the tiles done.
    var bytes: Int64 = 0
    /// Bytes downloaded in this run.
    var downloadedBytes: Int64 = 0
    /// Index of the next tile not yet started (the rest of the list is untouched).
    var cursor = 0

    var fraction: Double { total > 0 ? min(1, Double(done + failed) / Double(total)) : 1 }
}

/// The end of one run of `TileDownloader.run`.
struct TileDownloadSummary: Equatable, Sendable {
    var progress: TileDownloadProgress
    /// Set when the network, Low Data Mode or storage stopped it.
    var stop: TileDownloadStop?
    var cancelled: Bool

    /// Every tile was dealt with (some may have failed).
    var isComplete: Bool { stop == nil && !cancelled }
}

/// Downloads terrain tiles straight into the pinned store (docs/design/explorer.md, Download
/// engine): `policy.concurrency` requests at once (8), 15 s timeouts, retries after 0.5, 2 and 8 s
/// for transient failures (timeouts, dropped connections, 5xx, 429), 404 = no terrain there.
/// Resuming is running again: tiles already pinned are skipped without a request, and a tile in
/// the terrain cache is moved, not downloaded again. Offline, mobile data (when not allowed), Low
/// Data Mode and a full disk stop the whole run with a reason instead of failing every tile.
/// Cancel the calling task to pause. Thread-safe; `run` is called from a background task.
final class TileDownloader: @unchecked Sendable {
    struct Policy: Equatable, Sendable {
        var concurrency = 8
        var timeout: TimeInterval = 15
        var retryDelays: [Double] = [0.5, 2, 8]
        /// May use mobile data or a personal hotspot.
        var allowsExpensive = false
        /// May run in Low Data Mode.
        var allowsConstrained = false
        /// Shortest pause between two progress reports (seconds).
        var reportInterval: TimeInterval = 0.25
    }

    let pinned: PinnedTerrainStore
    /// The terrain cache (`DEMService.cacheURL` layout) whose tiles are moved instead of downloaded.
    let cacheDirectory: URL?
    let policy: Policy
    private let session: URLSession

    init(pinned: PinnedTerrainStore, cacheDirectory: URL?, session: URLSession? = nil, policy: Policy = Policy()) {
        self.pinned = pinned
        self.cacheDirectory = cacheDirectory
        self.policy = policy
        self.session = session ?? Self.makeSession(policy: policy)
    }

    /// A session for tile downloads: no URL cache (tiles go to the pinned store), the policy's
    /// timeout, as many connections as requests at once.
    static func makeSession(policy: Policy = Policy()) -> URLSession {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = policy.timeout
        config.timeoutIntervalForResource = policy.timeout * 2
        config.httpMaximumConnectionsPerHost = policy.concurrency
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.urlCache = nil
        return URLSession(configuration: config)
    }

    /// The same downloader with another policy (and the same session).
    func with(_ policy: Policy) -> TileDownloader {
        TileDownloader(pinned: pinned, cacheDirectory: cacheDirectory, session: session, policy: policy)
    }

    // MARK: - Run

    /// Downloads `tiles` in order (skipping those already pinned) until all are dealt with, the
    /// network or storage stops it, or the calling task is cancelled. `progress` is called at most
    /// every `policy.reportInterval` and once at the end, from the calling task.
    func run(_ tiles: [TileID], progress: @Sendable (TileDownloadProgress) -> Void) async -> TileDownloadSummary {
        var state = TileDownloadProgress(total: tiles.count)
        var stop: TileDownloadStop?
        var cancelled = false
        var lastReport = Date.distantPast
        let width = max(1, policy.concurrency)

        await withTaskGroup(of: TileFetchOutcome.self) { group in
            var running = 0
            /// Starts the next tiles that need a request; tiles already pinned are counted at once
            /// (a resumed region skips thousands of them in a second or two).
            func fill() {
                while running < width, state.cursor < tiles.count {
                    let t = tiles[state.cursor]
                    state.cursor += 1
                    if let size = pinned.tileBytes(t) {
                        state.done += 1
                        state.bytes += size
                        continue
                    }
                    running += 1
                    group.addTask { await self.fetch(t) }
                }
            }
            fill()
            while let outcome = await group.next() {
                running -= 1
                switch outcome {
                case .downloaded(let bytes):
                    state.done += 1
                    state.bytes += bytes
                    state.downloadedBytes += bytes
                case .present(let bytes), .adopted(let bytes):
                    state.done += 1
                    state.bytes += bytes
                case .noData:
                    state.done += 1
                case .failed:
                    state.failed += 1
                case .stop(let reason):
                    stop = stop ?? reason
                case .cancelled:
                    cancelled = true
                }
                if Date().timeIntervalSince(lastReport) >= policy.reportInterval {
                    lastReport = Date()
                    progress(state)
                }
                if stop != nil || cancelled || Task.isCancelled {
                    group.cancelAll()
                    continue
                }
                fill()
            }
        }
        // Tiles started but stopped early are not counted: the next run finds them absent and
        // fetches them again.
        progress(state)
        return TileDownloadSummary(progress: state, stop: stop, cancelled: stop == nil && (cancelled || Task.isCancelled))
    }

    // MARK: - One tile

    /// Pins one tile: already there, moved from the cache, or downloaded with retries.
    func fetch(_ t: TileID) async -> TileFetchOutcome {
        if let size = pinned.tileBytes(t) { return .present(size) }
        if let adopted = adoptFromCache(t) { return .adopted(adopted) }
        guard let url = DEMService.remoteURL(for: t) else { return .noData }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: policy.timeout)
        request.allowsExpensiveNetworkAccess = policy.allowsExpensive
        request.allowsConstrainedNetworkAccess = policy.allowsConstrained
        var attempt = 0
        while true {
            if Task.isCancelled { return .cancelled }
            let retryable: Bool
            do {
                let (data, response) = try await session.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 200, DEMService.isPNG(data) {
                    // The region was deleted meanwhile: nothing may be written for it.
                    guard pinned.covers(t) else { return .cancelled }
                    do {
                        // Not kept: the region was deleted while the tile was on its way.
                        guard try pinned.store(data, for: t) else { return .cancelled }
                        return .downloaded(Int64(data.count))
                    } catch {
                        return Self.isOutOfSpace(error) ? .stop(.storageFull) : .failed
                    }
                }
                if status == 404 { return .noData }
                // 200 with something else: a captive portal or a proxy page; worth another try.
                retryable = status == 200 || DEMService.isRetryable(status: status)
            } catch {
                if Task.isCancelled { return .cancelled }
                if let reason = Self.stopReason(error) { return .stop(reason) }
                // A request cancelled by anything but this task (an invalidated session) fails
                // the tile; the next run tries it again.
                retryable = DEMService.isRetryable(error)
            }
            guard retryable, attempt < policy.retryDelays.count else { return .failed }
            let delay = max(0, policy.retryDelays[attempt])
            attempt += 1
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
        }
    }

    /// Moves a valid tile from the terrain cache into the pinned store; its bytes, or nil.
    private func adoptFromCache(_ t: TileID) -> Int64? {
        guard let cacheDirectory else { return nil }
        let file = cacheDirectory
            .appendingPathComponent("\(t.z)", isDirectory: true)
            .appendingPathComponent("\(t.x)", isDirectory: true)
            .appendingPathComponent("\(t.y).png")
        guard pinned.covers(t), let data = try? Data(contentsOf: file), DEMService.isPNG(data) else { return nil }
        guard pinned.adopt(file, for: t) else { return nil }
        return Int64(data.count)
    }

    // MARK: - Measuring

    /// Sizes of `tiles` on the server (HEAD requests, all at once, 8 s timeouts); tiles that did
    /// not answer are absent. Used to measure a region's download before it starts.
    func measure(_ tiles: [TileID]) async -> [TileID: Int64] {
        var out: [TileID: Int64] = [:]
        await withTaskGroup(of: (TileID, Int64?).self) { group in
            for t in Set(tiles) {
                group.addTask {
                    guard let url = DEMService.remoteURL(for: t) else { return (t, nil) }
                    var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 8)
                    request.httpMethod = "HEAD"
                    request.allowsExpensiveNetworkAccess = true
                    request.allowsConstrainedNetworkAccess = true
                    guard let answer = try? await self.session.data(for: request),
                          let http = answer.1 as? HTTPURLResponse, http.statusCode == 200
                    else { return (t, nil) }
                    let length = http.expectedContentLength > 0
                        ? http.expectedContentLength
                        : Int64(http.value(forHTTPHeaderField: "Content-Length") ?? "") ?? -1
                    return (t, length > 0 ? length : nil)
                }
            }
            for await (t, size) in group {
                if let size { out[t] = size }
            }
        }
        return out
    }

    // MARK: - Errors

    /// Errors that stop the whole download rather than one tile.
    static func stopReason(_ error: Error) -> TileDownloadStop? {
        guard let error = error as? URLError else { return isOutOfSpace(error) ? .storageFull : nil }
        if let reason = error.networkUnavailableReason {
            switch reason {
            case .constrained: return .lowDataMode
            case .expensive, .cellular: return .wifi
            @unknown default: return .offline
            }
        }
        switch error.code {
        case .notConnectedToInternet, .dataNotAllowed, .internationalRoamingOff, .callIsActive:
            return .offline
        default:
            return nil
        }
    }

    /// A write failed because the disk is full.
    static func isOutOfSpace(_ error: Error) -> Bool {
        let ns = error as NSError
        if ns.domain == NSCocoaErrorDomain, ns.code == NSFileWriteOutOfSpaceError { return true }
        if ns.domain == NSPOSIXErrorDomain, ns.code == Int(ENOSPC) { return true }
        if let underlying = ns.userInfo[NSUnderlyingErrorKey] as? NSError {
            return isOutOfSpace(underlying)
        }
        return false
    }
}
