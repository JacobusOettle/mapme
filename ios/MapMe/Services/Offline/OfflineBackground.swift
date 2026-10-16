import BackgroundTasks
import Foundation
import UIKit

/// How a region download carries on when MapMe leaves the screen (docs/design/explorer.md,
/// Download engine, "in the background"). The settings list says which applies.
enum OfflineBackgroundMode: Equatable, Sendable {
    /// In front, or nothing downloading.
    case none
    /// iOS 26: a continued-processing task with the system's progress UI; the download runs on.
    case continuedProcessing
    /// iOS 17–18 (or iOS 26 declined): the ~30 s iOS grants an app leaving the screen, then the
    /// next tiles are handed to a background URLSession and the region pauses until MapMe is back.
    case shortGrace
    /// The next tiles were handed to the background URLSession; iOS fetches them while MapMe is
    /// suspended.
    case handedOff(tiles: Int)
}

// MARK: - iOS 26: continued processing

/// The iOS 26 continued-processing task that keeps an offline download going after the user
/// leaves MapMe, with the system's progress UI (a Live Activity the user can stop). Submitted
/// from the foreground when the user starts or resumes a download; the work itself is the
/// download already running, so the launch handler only keeps the task to report progress and
/// to complete it.
///
/// The identifier is a fixed one (`project.yml`, BGTaskSchedulerPermittedIdentifiers), registered
/// once with that exact identifier just before the first submit: Apple's "Performing long-running
/// tasks on iOS and iPadOS" registers its handler that way, on demand rather than at launch, and
/// asks for no background mode. One region downloads at a time, so one task at a time is enough. A
/// request iOS could not start at once (`strategy = .fail`), or one that starts after the queue
/// emptied, is completed straight away.
@MainActor
final class OfflineContinuedProcessing {
    static let shared = OfflineContinuedProcessing()
    static let identifier = "com.jacobusoettle.mapme.offline-download"

    /// The system stopped the task (the user tapped stop, or iOS needed the resources).
    var onExpired: (() -> Void)?
    /// A task is granted and running.
    private(set) var isActive = false
    /// A task was asked for (it may not have started yet) and not finished.
    var isRequested: Bool { isActive || submitted }

    private var registered = false
    private var submitted = false
    /// The `BGContinuedProcessingTask` (typed loosely: the class only exists on iOS 26).
    private var task: AnyObject?

    private init() {}

    /// Asks iOS 26 to let the download continue in the background. Call in response to the user
    /// (Download, Resume) while MapMe is in front. False where unavailable (iOS 17–18, the
    /// simulator, refused).
    @discardableResult
    func begin(title: String, subtitle: String) -> Bool {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            if isActive || submitted { return true }
            registerIfNeeded()
            guard registered else { return false }
            let request = BGContinuedProcessingTaskRequest(identifier: Self.identifier, title: title, subtitle: subtitle)
            // Start now or not at all: a request that waits would start after the download did.
            request.strategy = .fail
            do {
                try BGTaskScheduler.shared.submit(request)
                submitted = true
                return true
            } catch {
                return false
            }
        }
        #endif
        return false
    }

    /// Progress (0…1) and the line under the title ("Lesotho · 1 204 of 2 475 tiles").
    func update(fraction: Double, subtitle: String) {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *), let task = task as? BGContinuedProcessingTask {
            task.progress.totalUnitCount = 1_000
            task.progress.completedUnitCount = Int64((min(max(fraction, 0), 1) * 1_000).rounded())
            task.updateTitle(task.title, subtitle: subtitle)
        }
        #endif
    }

    /// The queue is done (or paused): let the system end the task.
    func finish(success: Bool) {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *), let task = task as? BGContinuedProcessingTask {
            task.setTaskCompleted(success: success)
        }
        #endif
        task = nil
        isActive = false
        submitted = false
    }

    #if compiler(>=6.2)
    @available(iOS 26.0, *)
    private func registerIfNeeded() {
        guard !registered else { return }
        // Handlers run on the main queue: the task is kept and updated on the main actor.
        registered = BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.identifier, using: DispatchQueue.main) { task in
            MainActor.assumeIsolated {
                OfflineContinuedProcessing.shared.started(task)
            }
        }
    }

    @available(iOS 26.0, *)
    private func started(_ bgTask: BGTask) {
        guard let continued = bgTask as? BGContinuedProcessingTask else {
            bgTask.setTaskCompleted(success: false)
            return
        }
        // Finished (or never wanted) before iOS got round to starting it: nothing to keep going.
        guard submitted else {
            continued.setTaskCompleted(success: true)
            return
        }
        task = continued
        isActive = true
        continued.progress.totalUnitCount = 1_000
        // Called on a queue of the system's choosing: hop to the main actor.
        continued.expirationHandler = { @Sendable in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    OfflineContinuedProcessing.shared.expired()
                }
            }
        }
    }
    #endif

    private func expired() {
        let handler = onExpired
        finish(success: false)
        handler?()
    }
}

// MARK: - iOS 17–18: background URLSession

/// The next tiles of a region handed to a background URLSession when MapMe's background time
/// runs out (iOS 17–18, or iOS 26 without a continued-processing task): iOS's download daemon
/// fetches them while MapMe is suspended, and each finished file is moved into the pinned store
/// as it is delivered (when iOS wakes MapMe for the session, or at the next launch). Back in front,
/// the rest is cancelled and the foreground engine resumes, skipping what arrived.
///
/// A background session suits a bounded batch of files, not tens of thousands of small requests
/// (iOS spaces out the wake-ups of an app that schedules many), so at most `maxHandoff` tiles go
/// per stay in the background.
final class BackgroundTileTransfer: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    static let identifier = "com.jacobusoettle.mapme.offline-tiles"
    static let shared = BackgroundTileTransfer()
    /// Tiles handed over per stay in the background (~50–120 MB).
    static let maxHandoff = 1_000
    /// UserDefaults key: tiles were handed over and not collected yet.
    static let pendingKey = "offlineBackgroundHandoffPending"

    private let lock = NSLock()
    private var pinned: PinnedTerrainStore = .shared
    private var _session: URLSession?
    private var delivered = 0
    /// Called (main queue) when iOS has delivered every event of the session.
    var onEventsFinished: (() -> Void)?

    private var session: URLSession {
        lock.withLock {
            if let s = _session { return s }
            let config = URLSessionConfiguration.background(withIdentifier: Self.identifier)
            config.sessionSendsLaunchEvents = true
            config.isDiscretionary = false
            config.allowsConstrainedNetworkAccess = false
            let s = URLSession(configuration: config, delegate: self, delegateQueue: nil)
            _session = s
            return s
        }
    }

    /// Where delivered tiles go (the offline feature's store).
    func use(_ store: PinnedTerrainStore) {
        lock.withLock { pinned = store }
    }

    /// Recreates the session so downloads that finished while MapMe was not running are
    /// delivered. Cheap when nothing is pending.
    func reconnectIfPending() {
        guard UserDefaults.standard.bool(forKey: Self.pendingKey) else { return }
        _ = session
    }

    /// Hands `tiles` to iOS's download daemon. `allowsExpensive`: may use mobile data.
    func enqueue(_ tiles: [TileID], allowsExpensive: Bool) {
        guard !tiles.isEmpty else { return }
        let session = self.session
        for t in tiles.prefix(Self.maxHandoff) {
            guard let url = DEMService.remoteURL(for: t) else { continue }
            var request = URLRequest(url: url)
            request.allowsExpensiveNetworkAccess = allowsExpensive
            request.allowsConstrainedNetworkAccess = false
            let task = session.downloadTask(with: request)
            task.taskDescription = "\(t.z)/\(t.x)/\(t.y)"
            task.resume()
        }
        UserDefaults.standard.set(true, forKey: Self.pendingKey)
    }

    /// Cancels what iOS has not fetched yet (MapMe is in front again; the foreground engine is
    /// faster). Returns how many tiles were delivered since the hand-off.
    @discardableResult
    func cancelPending() async -> Int {
        guard UserDefaults.standard.bool(forKey: Self.pendingKey) else { return 0 }
        let tasks = await session.allTasks
        for task in tasks { task.cancel() }
        UserDefaults.standard.set(false, forKey: Self.pendingKey)
        return lock.withLock {
            let n = delivered
            delivered = 0
            return n
        }
    }

    /// "z/x/y" of a task's description.
    static func tile(from description: String?) -> TileID? {
        guard let parts = description?.split(separator: "/"), parts.count == 3,
              let z = Int(parts[0]), let x = Int(parts[1]), let y = Int(parts[2])
        else { return nil }
        return TileID(z: z, x: x, y: y)
    }

    // MARK: URLSessionDownloadDelegate

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        // The file is deleted when this returns: move it now.
        guard let t = Self.tile(from: downloadTask.taskDescription),
              let http = downloadTask.response as? HTTPURLResponse, http.statusCode == 200
        else { return }
        let store = lock.withLock { pinned }
        // A region deleted meanwhile no longer covers it.
        guard store.covers(t), Self.startsWithPNGSignature(location) else { return }
        if store.adopt(location, for: t) {
            lock.withLock { delivered += 1 }
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {}

    func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        DispatchQueue.main.async { [weak self] in
            self?.onEventsFinished?()
        }
    }

    private static func startsWithPNGSignature(_ url: URL) -> Bool {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return false }
        defer { try? handle.close() }
        let head = (try? handle.read(upToCount: 9)) ?? Data()
        return DEMService.isPNG(head)
    }
}
