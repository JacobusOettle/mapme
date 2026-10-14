import UIKit

/// Waits for UIKit presentations to finish before the app presents a full-screen cover of its own.
///
/// A cover that SwiftUI presents while another presentation is still opening or closing (the
/// photo picker or the Files browser closing after a pick, a context menu fading out) can be laid
/// out with a zero safe area, its chrome under the status bar. The import cover is therefore
/// presented only once nothing in the app's windows is in a presentation or dismissal transition.
@MainActor
enum PresentationGate {
    /// Transitions waited for at most (one closing and one opening are the usual worst case).
    static let maxTransitions = 4
    /// Never wait longer than this for one transition, even if UIKit never reports its end.
    static let maxTransitionWait: TimeInterval = 2

    /// Returns once no view controller in the app's windows is being presented or dismissed.
    static func settled() async {
        // Let SwiftUI start the dismissals it was asked for in the update that led here.
        await nextMainQueueTurn()
        for _ in 0..<maxTransitions {
            guard let coordinator = runningTransition() else { return }
            await finish(coordinator)
            await nextMainQueueTurn()
        }
    }

    /// The coordinator of a presentation or dismissal under way in any of the app's windows.
    static func runningTransition() -> UIViewControllerTransitionCoordinator? {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap { $0.windows }
        for window in windows {
            var controller = window.rootViewController
            while let current = controller {
                if let coordinator = current.transitionCoordinator { return coordinator }
                controller = current.presentedViewController
            }
        }
        return nil
    }

    private static func finish(_ coordinator: UIViewControllerTransitionCoordinator) async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let once = ResumeOnce(continuation)
            let queued = coordinator.animate(alongsideTransition: nil) { _ in once.resume() }
            if !queued {
                once.resume()
                return
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + maxTransitionWait) { once.resume() }
        }
    }

    private static func nextMainQueueTurn() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.main.async { continuation.resume() }
        }
    }
}

/// Resumes a continuation the first time it is asked to; later calls do nothing. Thread-safe.
private final class ResumeOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?

    init(_ continuation: CheckedContinuation<Void, Never>) {
        self.continuation = continuation
    }

    func resume() {
        let pending: CheckedContinuation<Void, Never>? = lock.withLock {
            defer { continuation = nil }
            return continuation
        }
        pending?.resume()
    }
}
