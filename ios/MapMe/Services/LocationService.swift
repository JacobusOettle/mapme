import CoreLocation
import Foundation
import Observation
import UIKit

/// CoreLocation wrapper: position + compass heading.
/// Launch arguments for tests/screenshots: `-mockLocation <lat>,<lon>`, `-mockHeading <deg>`, `-mockWalk`,
/// `-mockWalkSpeed <m/s>` (default 1.5).
/// As the host app of the unit tests (and not mocked) it stays idle: no CoreLocation, no permission
/// request, so no system alert outlives the unit tests and covers the UI tests that follow.
/// Use from the main thread (delegate callbacks arrive on main because the manager is created there).
@Observable
final class LocationService: NSObject {
    /// Latest fix (nil until the first one arrives).
    private(set) var location: CLLocation?
    /// True heading in degrees 0..<360 (magnetic if true is unavailable); nil without a compass.
    private(set) var headingDegrees: Double?
    /// Heading uncertainty in degrees, if known.
    private(set) var headingAccuracy: Double?
    private(set) var authorization: CLAuthorizationStatus = .notDetermined
    /// True when driven by launch-argument mocks.
    private(set) var isMocked = false
    /// True when CoreLocation is left alone: the app is hosting unit tests without mocks.
    private(set) var isIdleForTests = false

    /// The app was launched by XCTest to host unit tests (XCTest puts its configuration in the
    /// host's environment; apps launched by UI tests do not get it).
    static var isHostingUnitTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    @ObservationIgnored private var manager: CLLocationManager?
    @ObservationIgnored private var observers: [(CLLocation) -> Void] = []
    @ObservationIgnored private var running = false
    @ObservationIgnored private var backgroundUpdates = false
    @ObservationIgnored private var orientationObserver: NSObjectProtocol?

    @ObservationIgnored private var mock: Mock?
    @ObservationIgnored private var mockTimer: Timer?

    private struct Mock {
        var start: LatLon
        var heading: Double?
        var walk: Bool
        /// Walking speed in m/s.
        var speed: Double = 1.5
        var elapsed: TimeInterval = 0
        var position: LatLon
    }

    override init() {
        super.init()
        if let mock = Self.parseMock(ProcessInfo.processInfo.arguments) {
            self.mock = mock
            isMocked = true
            authorization = .authorizedWhenInUse
        } else if Self.isHostingUnitTests {
            isIdleForTests = true
        } else {
            let m = CLLocationManager()
            m.delegate = self
            m.desiredAccuracy = kCLLocationAccuracyBest
            m.activityType = .fitness
            m.distanceFilter = kCLDistanceFilterNone
            m.headingFilter = 1
            m.pausesLocationUpdatesAutomatically = false
            manager = m
            authorization = m.authorizationStatus
        }
    }

    deinit {
        mockTimer?.invalidate()
        if let orientationObserver { NotificationCenter.default.removeObserver(orientationObserver) }
    }

    func requestPermission() {
        guard let manager, manager.authorizationStatus == .notDetermined else { return }
        manager.requestWhenInUseAuthorization()
    }

    func start() {
        guard !running else { return }
        running = true
        if mock != nil {
            startMock()
            return
        }
        guard let manager else { return }
        updateHeadingOrientation()
        if orientationObserver == nil {
            UIDevice.current.beginGeneratingDeviceOrientationNotifications()
            orientationObserver = NotificationCenter.default.addObserver(
                forName: UIDevice.orientationDidChangeNotification, object: nil, queue: .main
            ) { [weak self] _ in
                self?.updateHeadingOrientation()
            }
        }
        if isAuthorized(manager.authorizationStatus) {
            manager.startUpdatingLocation()
        }
        if CLLocationManager.headingAvailable() {
            manager.startUpdatingHeading()
        }
    }

    func stop() {
        guard running else { return }
        running = false
        mockTimer?.invalidate()
        mockTimer = nil
        guard let manager else { return }
        if !backgroundUpdates {
            manager.stopUpdatingLocation()
        }
        manager.stopUpdatingHeading()
    }

    /// Keep receiving locations in the background (track recording).
    func setBackgroundUpdates(_ enabled: Bool) {
        backgroundUpdates = enabled
        guard let manager else { return }
        if enabled, manager.authorizationStatus == .authorizedWhenInUse {
            manager.requestAlwaysAuthorization()
        }
        manager.allowsBackgroundLocationUpdates = enabled
        manager.showsBackgroundLocationIndicator = enabled
        if enabled, isAuthorized(manager.authorizationStatus) {
            manager.startUpdatingLocation()
        }
    }

    /// Register a callback for every new location (called on the main thread).
    func addLocationObserver(_ observer: @escaping (CLLocation) -> Void) {
        observers.append(observer)
        if let location { observer(location) }
    }

    // MARK: - Internals

    private func isAuthorized(_ status: CLAuthorizationStatus) -> Bool {
        status == .authorizedWhenInUse || status == .authorizedAlways
    }

    private func publish(_ fix: CLLocation) {
        location = fix
        for observer in observers { observer(fix) }
    }

    /// Keep the compass reference in step with the interface so the heading cone is right in landscape.
    private func updateHeadingOrientation() {
        guard let manager else { return }
        let orientation: CLDeviceOrientation
        switch UIDevice.current.orientation {
        case .portrait: orientation = .portrait
        case .portraitUpsideDown:
            // iPhone UI never turns upside down, so the compass must follow the interface.
            orientation = UIDevice.current.userInterfaceIdiom == .pad ? .portraitUpsideDown : Self.interfaceDeviceOrientation()
        case .landscapeLeft: orientation = .landscapeLeft
        case .landscapeRight: orientation = .landscapeRight
        case .faceUp, .faceDown, .unknown:
            // Flat on a table: fall back to the interface orientation.
            orientation = Self.interfaceDeviceOrientation()
        @unknown default:
            orientation = .portrait
        }
        if manager.headingOrientation != orientation {
            manager.headingOrientation = orientation
        }
    }

    private static func interfaceDeviceOrientation() -> CLDeviceOrientation {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        switch scene?.interfaceOrientation {
        case .landscapeLeft?: return .landscapeRight
        case .landscapeRight?: return .landscapeLeft
        case .portraitUpsideDown?: return .portraitUpsideDown
        default: return .portrait
        }
    }

    // MARK: - Mock mode

    private static func parseMock(_ args: [String]) -> Mock? {
        guard let i = args.firstIndex(of: "-mockLocation"), i + 1 < args.count else { return nil }
        let parts = args[i + 1].split(separator: ",").map { Double($0.trimmingCharacters(in: .whitespaces)) }
        guard parts.count == 2, let lat = parts[0], let lon = parts[1] else { return nil }
        var heading: Double?
        if let h = args.firstIndex(of: "-mockHeading"), h + 1 < args.count, let deg = Double(args[h + 1]) {
            heading = deg
        }
        var speed = 1.5
        if let s = args.firstIndex(of: "-mockWalkSpeed"), s + 1 < args.count, let v = Double(args[s + 1]), v.isFinite, v > 0 {
            speed = min(v, 50)
        }
        let start = LatLon(lat: lat, lon: lon)
        return Mock(start: start, heading: heading, walk: args.contains("-mockWalk"), speed: speed, position: start)
    }

    private func startMock() {
        guard let mock else { return }
        headingDegrees = mock.heading
        headingAccuracy = mock.heading == nil ? nil : 10
        publishMock()
        guard mock.walk else { return }
        let timer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            self?.advanceMock(by: 1)
        }
        RunLoop.main.add(timer, forMode: .common)
        mockTimer = timer
    }

    /// Walks a gentle loop (`-mockWalkSpeed`, default 1.5 m/s; 300 m radius) that starts at the mock position.
    private func advanceMock(by dt: TimeInterval) {
        guard var mock else { return }
        mock.elapsed += dt
        let radius = 300.0
        let speed = mock.speed
        let theta = mock.elapsed * speed / radius
        // Circle centre lies due east of the start, so the walk begins heading north.
        let centre = Geodesy.destination(mock.start, bearing: 90, distance: radius)
        let bearingFromCentre = (270 + theta * 180 / .pi).truncatingRemainder(dividingBy: 360)
        let next = Geodesy.destination(centre, bearing: bearingFromCentre, distance: radius)
        let travel = Geodesy.bearing(mock.position, next)
        mock.position = next
        mock.heading = travel
        self.mock = mock
        headingDegrees = travel
        publishMock(course: travel, speed: speed)
    }

    private func publishMock(course: Double = -1, speed: Double = 0) {
        guard let mock else { return }
        let fix = CLLocation(coordinate: mock.position.coordinate, altitude: 1500,
                             horizontalAccuracy: 5, verticalAccuracy: 5,
                             course: course, speed: speed, timestamp: Date())
        publish(fix)
    }
}

extension LocationService: CLLocationManagerDelegate {
    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        authorization = manager.authorizationStatus
        if running, isAuthorized(manager.authorizationStatus) {
            manager.startUpdatingLocation()
        }
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        for fix in locations where fix.horizontalAccuracy >= 0 {
            publish(fix)
        }
    }

    func locationManager(_ manager: CLLocationManager, didUpdateHeading newHeading: CLHeading) {
        guard newHeading.headingAccuracy >= 0 else { return }
        headingDegrees = newHeading.trueHeading >= 0 ? newHeading.trueHeading : newHeading.magneticHeading
        headingAccuracy = newHeading.headingAccuracy
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        // kCLErrorLocationUnknown is transient; denial is reported through the authorization callback.
    }

    func locationManagerShouldDisplayHeadingCalibration(_ manager: CLLocationManager) -> Bool {
        (headingAccuracy ?? 180) > 30
    }
}
