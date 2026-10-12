import Foundation
import Network
import Observation

/// The current network path (`NWPathMonitor`): online or not, and whether it is expensive
/// (mobile data, a personal hotspot) or constrained (Low Data Mode). The "Search wider" sheet asks
/// before a big download on an expensive path; place-name lookups are skipped offline.
@Observable
final class NetworkPathObserver {
    /// Started on first use and kept for the app's lifetime.
    static let shared: NetworkPathObserver = {
        let observer = NetworkPathObserver()
        observer.start()
        return observer
    }()

    /// Thread-safe snapshot for background work. True until the first path update says otherwise.
    static var isOnlineNow: Bool { shared.snapshot.online }

    private(set) var isOnline = true
    private(set) var isExpensive = false
    private(set) var isConstrained = false

    @ObservationIgnored private var monitor: NWPathMonitor?
    private let lock = NSLock()
    @ObservationIgnored private var latest = (online: true, expensive: false, constrained: false)

    /// Latest path as seen from any thread.
    var snapshot: (online: Bool, expensive: Bool, constrained: Bool) {
        lock.withLock { latest }
    }

    func start() {
        guard monitor == nil else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            let value = (online: path.status == .satisfied, expensive: path.isExpensive, constrained: path.isConstrained)
            self?.lock.withLock { self?.latest = value }
            DispatchQueue.main.async { [weak self] in
                self?.isOnline = value.online
                self?.isExpensive = value.expensive
                self?.isConstrained = value.constrained
            }
        }
        monitor.start(queue: DispatchQueue(label: "com.jacobusoettle.mapme.network-path", qos: .utility))
        self.monitor = monitor
    }

    deinit {
        monitor?.cancel()
    }
}
