import CoreLocation
import Foundation
import Observation

/// Where you are along the route you follow.
struct RouteProgress: Equatable, Sendable {
    /// Along the route, from its start to the point nearest you.
    var distanceDoneM: Double
    var distanceRemainingM: Double
    /// From the route's profile, beyond the point nearest you (nil without terrain).
    var ascentRemainingM: Double?
    var descentRemainingM: Double?
    /// From you to the route.
    var distanceFromRouteM: Double
    /// Initial bearing from you to the nearest route point (degrees), for "back to the route".
    var bearingToRouteDegrees: Double?
    /// Moving time left at your pace (`PaceModel`).
    var remainingTime: TimeInterval?
    /// Now + `remainingTime`.
    var eta: Date?
    /// The route's next named point (an anchor's name, a GPX rtept name) or its end.
    var nextName: String?
}

/// Following a route: progress along it, off-route warnings (haptic, banner, a notification when
/// MapMe is in the background), arrival, and the ETA against sunset (docs/design/explorer.md,
/// Routes: Follow). Use from the main thread.
@MainActor
@Observable
final class RouteNavigator {
    /// The route being followed (nil when none).
    private(set) var activeRouteID: UUID?
    private(set) var progress: RouteProgress?
    /// Off the route for longer than the dwell, by more than the threshold.
    private(set) var isOffRoute = false
    /// Within reach of the route's end.
    private(set) var hasArrived = false

    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Starts following `route` (asks for notification permission the first time; keeps location
    /// updates running in the background with reason "routeFollow").
    func follow(_ route: Route) {
        // Filled in by the routes feature.
    }

    func stop() {
        // Filled in by the routes feature.
    }
}
