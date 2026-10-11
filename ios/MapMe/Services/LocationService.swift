// OWNER: map agent. Replace the implementation; keep this public API.
import CoreLocation
import Foundation
import Observation

/// CoreLocation wrapper: position + compass heading.
/// Launch arguments for tests/screenshots: `-mockLocation <lat>,<lon>`, `-mockHeading <deg>`, `-mockWalk`.
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

    func requestPermission() {}
    func start() {}
    func stop() {}
    /// Keep receiving locations in the background (track recording).
    func setBackgroundUpdates(_ enabled: Bool) {}
    /// Register a callback for every new location (called on the main thread).
    func addLocationObserver(_ observer: @escaping (CLLocation) -> Void) {}
}
