import CoreLocation
import Foundation
import Observation
import UIKit
import UserNotifications

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
    /// Metres along the route to `nextName`.
    var nextDistanceM: Double? = nil
    /// `remainingTime` assumed flat ground (no terrain for the route).
    var timeIsFlatEstimate = false
    /// Where the daylight banner says you arrive (the last named point ahead, else the end) and
    /// when, at your pace.
    var destinationName: String? = nil
    var destinationETA: Date? = nil
}

/// The off-route rule (docs/design/explorer.md, Follow): off once the distance to the route stays
/// above T = max(40 m, 2 × the fix's accuracy) for 20 s and two fixes in a row; back on below
/// 0.6 T. Pure, so it is tested on synthetic fixes.
struct OffRouteDetector: Equatable, Sendable {
    static let minThresholdM = 40.0
    static let accuracyMultiple = 2.0
    static let dwell: TimeInterval = 20
    static let fixesInARow = 2
    static let backOnFraction = 0.6

    enum Event: Equatable, Sendable {
        /// Nothing changed.
        case unchanged
        /// Just went off the route.
        case left
        /// Just came back.
        case rejoined
    }

    private(set) var isOffRoute = false
    /// The first fix of the current run beyond the threshold.
    private(set) var overSince: Date?
    private(set) var overCount = 0

    /// T for a fix of this horizontal accuracy (metres; nil or negative: unknown).
    static func threshold(accuracyM: Double?) -> Double {
        guard let a = accuracyM, a.isFinite, a > 0 else { return minThresholdM }
        return max(minThresholdM, accuracyMultiple * a)
    }

    mutating func update(distanceM: Double, accuracyM: Double?, at time: Date) -> Event {
        let t = Self.threshold(accuracyM: accuracyM)
        if distanceM > t {
            if overSince == nil {
                overSince = time
                overCount = 0
            }
            overCount += 1
            if !isOffRoute, overCount >= Self.fixesInARow, let since = overSince, time.timeIntervalSince(since) >= Self.dwell {
                isOffRoute = true
                return .left
            }
            return .unchanged
        }
        // A fix within the threshold breaks the run.
        overSince = nil
        overCount = 0
        if isOffRoute, distanceM < Self.backOnFraction * t {
            isOffRoute = false
            return .rejoined
        }
        return .unchanged
    }
}

/// Progress along one route as fixes come in: the nearest point (near the last progress first,
/// so loops and out-and-back routes do not jump), and the off-route state. Pure.
struct RouteTracker: Sendable {
    /// Searched around the last progress first (metres of route distance either way).
    static let searchWindowM = 500.0
    /// Within this of the end (along the route, or straight when most of it is done): arrived.
    static let arrivalRadiusM = 30.0

    let line: RouteLine
    private(set) var detector = OffRouteDetector()
    /// The last progress (metres along), nil before the first fix.
    private(set) var lastAlongM: Double?

    init(points: [LatLon]) {
        line = RouteLine(points)
    }

    var isOffRoute: Bool { detector.isOffRoute }

    /// The fix's place on the route and what changed.
    mutating func update(position: LatLon, accuracyM: Double?, at time: Date) -> (match: RouteMatch, event: OffRouteDetector.Event)? {
        guard let match = locate(position, accuracyM: accuracyM) else { return nil }
        lastAlongM = match.distanceAlongM
        let event = detector.update(distanceM: match.distanceFromLineM, accuracyM: accuracyM, at: time)
        return (match, event)
    }

    /// The nearest point: within ±500 m of the last progress, or anywhere when that is farther
    /// than the off-route threshold (you took a shortcut, or started elsewhere).
    func locate(_ position: LatLon, accuracyM: Double?) -> RouteMatch? {
        guard let last = lastAlongM else { return line.nearest(to: position) }
        let window = max(0, last - Self.searchWindowM)...(last + Self.searchWindowM)
        let near = line.nearest(to: position, alongRange: window, preferAfter: last)
        if let near, near.distanceFromLineM <= OffRouteDetector.threshold(accuracyM: accuracyM) { return near }
        guard let anywhere = line.nearest(to: position, preferAfter: last) else { return near }
        guard let near else { return anywhere }
        return anywhere.distanceFromLineM < near.distanceFromLineM ? anywhere : near
    }

    /// Arrived: within 30 m of the end along the route, or within 30 m of it in a straight line
    /// with most of the route behind you (a loop starts where it ends).
    func hasArrived(match: RouteMatch, position: LatLon) -> Bool {
        guard let end = line.points.last, line.length > 0 else { return false }
        if line.length - match.distanceAlongM <= Self.arrivalRadiusM, match.distanceFromLineM <= OffRouteDetector.minThresholdM {
            return true
        }
        return Geodesy.distance(position, end) <= Self.arrivalRadiusM && match.distanceAlongM >= 0.5 * line.length
    }
}

/// Routes UI state shared by the map screen's SwiftUI views and the canvas overlays (the lead's
/// `MapScreenContext` has no room for a feature's own state).
@MainActor
@Observable
final class RouteScreenState {
    /// A geo item picked outside the map screen's own views (Nearby, the GPX summary, the routes
    /// list): the map screen selects it once it shows.
    var selectionRequest: GeoItemRef?
    /// The point the elevation profile is scrubbed to, shown on the map.
    var scrubPoint: LatLon?
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
    /// The followed route's profile (terrain, else the file's elevations).
    private(set) var profile: RouteProfile?
    /// Your pace from recorded tracks (nil until worked out, or without tracks).
    private(set) var learnedPace: PaceEstimate?
    /// Shared with the map screen's views and overlays.
    let screen = RouteScreenState()

    /// Off-route notifications sent in the background this episode (one, then a reminder).
    static let reminderAfter: TimeInterval = 5 * 60

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored private var tracker: RouteTracker?
    @ObservationIgnored private var trackedHash: Int?
    @ObservationIgnored private var profileTask: Task<Void, Never>?
    @ObservationIgnored private var paceTask: Task<Void, Never>?
    @ObservationIgnored private var learnedFromTrackCount = -1
    @ObservationIgnored private var notifiedThisEpisode = 0
    @ObservationIgnored private var firstNotificationAt: Date?
    @ObservationIgnored private var askedForNotifications = false
    @ObservationIgnored private var routeWatch: ObservationLoop?
    /// Learns the pace again when a recording is saved or a track deleted.
    @ObservationIgnored private var tracksWatch: ObservationLoop?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
        // The location service calls back on the main thread (its manager lives there).
        model.location.addLocationObserver { [weak self] fix in
            MainActor.assumeIsolated {
                self?.ingest(fix)
            }
        }
        tracksWatch = ObservationLoop { [weak self] in
            guard let self, let model = self.model else { return }
            _ = model.tracks.tracks.count
            self.refreshLearnedPace()
        }
    }

    // MARK: - Following

    /// Starts following `route` (asks for notification permission the first time; keeps location
    /// updates running in the background with reason "routeFollow").
    func follow(_ route: Route) {
        guard let model, route.points.count >= 2 else { return }
        stop()
        activeRouteID = route.id
        startTracking(route)
        model.location.setBackgroundUpdates(true, for: "routeFollow")
        requestNotificationPermissionIfNeeded()
        // Edited or deleted while followed: follow the new line, or stop.
        routeWatch = ObservationLoop { [weak self] in
            guard let self, let model = self.model, let id = self.activeRouteID else { return }
            _ = model.geoItems.revision
            guard let current = model.geoItems.route(id: id) else {
                self.stop()
                return
            }
            if current.geometryHash != self.trackedHash { self.startTracking(current) }
        }
        if let fix = model.location.location { ingest(fix) }
    }

    func stop() {
        routeWatch?.cancel()
        routeWatch = nil
        profileTask?.cancel()
        profileTask = nil
        if activeRouteID != nil {
            model?.location.setBackgroundUpdates(false, for: "routeFollow")
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [Self.offRouteNotificationID])
        }
        activeRouteID = nil
        progress = nil
        isOffRoute = false
        hasArrived = false
        profile = nil
        tracker = nil
        trackedHash = nil
        notifiedThisEpisode = 0
        firstNotificationAt = nil
    }

    /// The route followed now, if it still exists.
    var activeRoute: Route? {
        guard let id = activeRouteID else { return nil }
        return model?.geoItems.route(id: id)
    }

    /// The pace used for ETAs now: Settings' custom factor, else the learned one, else Tobler's;
    /// with the last 45 min of this walk blended in.
    func currentPace(now: Date = RoutesClock.now()) -> PaceEstimate {
        guard let model else { return .model }
        let base: PaceEstimate
        if let custom = model.settings.walkingPaceFactor {
            base = PaceEstimate(factor: PaceModel.clamp(custom), source: .custom)
        } else {
            base = learnedPace ?? .model
        }
        guard activeRouteID != nil else { return base }
        let blended = PaceModel.blended(base: base.factor, recent: model.tracks.current.points, now: Date())
        return PaceEstimate(factor: blended, source: base.source)
    }

    /// Works the learned pace out again when the saved tracks changed (off the main thread).
    func refreshLearnedPace() {
        guard let model else { return }
        let tracks = model.tracks.tracks
        guard tracks.count != learnedFromTrackCount else { return }
        learnedFromTrackCount = tracks.count
        paceTask?.cancel()
        paceTask = Task { [weak self] in
            let pace = await Task.detached(priority: .utility) { PaceModel.learnedPace(from: tracks) }.value
            guard !Task.isCancelled else { return }
            self?.learnedPace = pace
        }
    }

    // MARK: - Internals

    private func startTracking(_ route: Route) {
        tracker = RouteTracker(points: route.latLons)
        trackedHash = route.geometryHash
        profile = nil
        profileTask?.cancel()
        guard let terrain = model?.terrain else { return }
        let snapshot = route
        profileTask = Task { [weak self] in
            let result = await RoutePlanner.bestProfile(for: snapshot, terrain: terrain)
            guard !Task.isCancelled, let self, self.activeRouteID == snapshot.id else { return }
            self.profile = result
            if let fix = self.model?.location.location { self.ingest(fix) }
        }
    }

    private func ingest(_ fix: CLLocation) {
        guard activeRouteID != nil, var tracker, let route = activeRoute else { return }
        let position = LatLon(fix.coordinate)
        let accuracy = fix.horizontalAccuracy >= 0 ? fix.horizontalAccuracy : nil
        let now = RoutesClock.now()
        guard let (match, event) = tracker.update(position: position, accuracyM: accuracy, at: fix.timestamp) else { return }
        self.tracker = tracker

        let line = tracker.line
        var p = RouteProgress(distanceDoneM: match.distanceAlongM,
                              distanceRemainingM: max(0, line.length - match.distanceAlongM),
                              ascentRemainingM: nil, descentRemainingM: nil,
                              distanceFromRouteM: match.distanceFromLineM,
                              bearingToRouteDegrees: match.distanceFromLineM > 1 ? Geodesy.bearing(position, match.point) : nil,
                              remainingTime: nil, eta: nil, nextName: nil)
        let pace = currentPace(now: now)
        let destination = Self.destination(route: route, line: line, after: match.distanceAlongM)
        // Moving time from `along` to the end of the route (metres along the line).
        var timeToEnd: (Double) -> TimeInterval = { along in
            PaceModel.flatTime(meters: line.length - along, factor: pace.factor)
        }
        if let profile, profile.hasElevation {
            // The profile's distances are the core's (or ours): the same haversine as the line.
            let scale = profile.profile.distanceM > 0 && line.length > 0 ? profile.profile.distanceM / line.length : 1
            if let climbs = RoutePlanner.remainingClimbs(of: profile.profile, fromDistanceM: match.distanceAlongM * scale) {
                p.ascentRemainingM = climbs.ascent
                p.descentRemainingM = climbs.descent
            }
            let shape = profile.profile
            timeToEnd = { along in
                PaceModel.movingTime(along: shape, factor: pace.factor, fromDistanceM: along * scale) ?? 0
            }
        } else {
            p.timeIsFlatEstimate = true
        }
        let remaining = timeToEnd(match.distanceAlongM)
        p.remainingTime = remaining
        p.eta = now.addingTimeInterval(remaining)
        p.destinationName = destination.name
        p.destinationETA = now.addingTimeInterval(max(0, remaining - timeToEnd(destination.alongM)))
        let next = Self.nextNamedPoint(route: route, line: line, after: match.distanceAlongM)
        p.nextName = next?.name
        p.nextDistanceM = next.map { max(0, $0.alongM - match.distanceAlongM) }
        progress = p

        switch event {
        case .left:
            isOffRoute = true
            notifiedThisEpisode = 0
            firstNotificationAt = nil
            if model?.settings.offRouteAlerts ?? true {
                UINotificationFeedbackGenerator().notificationOccurred(.warning)
            }
        case .rejoined:
            isOffRoute = false
            notifiedThisEpisode = 0
            firstNotificationAt = nil
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [Self.offRouteNotificationID])
        case .unchanged:
            break
        }
        if isOffRoute { notifyOffRouteIfInBackground(route: route, distanceM: match.distanceFromLineM) }

        if !hasArrived, tracker.hasArrived(match: match, position: position) {
            hasArrived = true
            UINotificationFeedbackGenerator().notificationOccurred(.success)
        }
    }

    /// The next point with a name ahead (an anchor's, a GPX rtept's), or the end.
    static func nextNamedPoint(route: Route, line: RouteLine, after alongM: Double) -> (name: String, alongM: Double)? {
        let points = route.points
        guard points.count == line.cumulative.count, !points.isEmpty else { return nil }
        for i in points.indices where line.cumulative[i] > alongM + 10 {
            if let name = points[i].name, !name.isEmpty { return (name, line.cumulative[i]) }
        }
        return (destinationName(of: route), line.length)
    }

    /// Where the route takes you: its last named point, else "the end of <route>".
    static func destinationName(of route: Route) -> String {
        if let name = route.points.last?.name, !name.isEmpty { return name }
        return "the end of \(route.name)"
    }

    /// Where the daylight banner says you arrive: the route's last named point still ahead (a hut
    /// before the walk out), else its end.
    static func destination(route: Route, line: RouteLine, after alongM: Double) -> (name: String, alongM: Double) {
        let points = route.points
        if points.count == line.cumulative.count {
            for i in points.indices.reversed() where line.cumulative[i] > alongM + 10 {
                if let name = points[i].name, !name.isEmpty { return (name, line.cumulative[i]) }
            }
        }
        return (destinationName(of: route), line.length)
    }

    // MARK: - Notifications

    private static let offRouteNotificationID = "routeFollow.offRoute"

    private func requestNotificationPermissionIfNeeded() {
        guard !askedForNotifications, let model, !model.location.isMocked, !LocationService.isHostingUnitTests else { return }
        askedForNotifications = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    /// Once per episode while MapMe is in the background, and once more after 5 min.
    private func notifyOffRouteIfInBackground(route: Route, distanceM: Double) {
        guard model?.settings.offRouteAlerts ?? true, UIApplication.shared.applicationState != .active else { return }
        let now = Date()
        if notifiedThisEpisode == 0 {
            firstNotificationAt = now
        } else if notifiedThisEpisode == 1, let first = firstNotificationAt, now.timeIntervalSince(first) >= Self.reminderAfter {
            // The reminder.
        } else {
            return
        }
        notifiedThisEpisode += 1
        let units = model?.settings.units ?? .metric
        let content = UNMutableNotificationContent()
        content.title = notifiedThisEpisode == 1 ? "Off route" : "Still off route"
        content.body = "You're \(GeoFormat.distance(distanceM, units: units)) off \(route.name)."
        content.sound = .default
        let request = UNNotificationRequest(identifier: Self.offRouteNotificationID, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { _ in }
    }
}
