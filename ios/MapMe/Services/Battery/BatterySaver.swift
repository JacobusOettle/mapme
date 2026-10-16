import CoreLocation
import Foundation
import Observation

/// CoreLocation settings for a power profile (applied by `LocationService.apply(_:)`).
struct LocationPowerProfile: Equatable, Sendable {
    var desiredAccuracy: CLLocationAccuracy
    var distanceFilter: CLLocationDistance
    var headingFilter: CLLocationDegrees

    /// As MapMe always ran: best accuracy, every fix, 1° heading steps.
    static let normal = LocationPowerProfile(desiredAccuracy: kCLLocationAccuracyBest,
                                             distanceFilter: kCLDistanceFilterNone, headingFilter: 1)
}

/// The battery saver (docs/design/explorer.md, Battery saver): what it changes is exactly what
/// iOS lets an app change (location accuracy and filters, heading updates, redraw rate,
/// animations, keeping the screen awake, background work), on with Low Power Mode by default.
/// Use from the main thread.
@MainActor
@Observable
final class BatterySaver {
    /// The saver is on now (the setting, or Low Power Mode with the setting on "auto").
    private(set) var isActive = false

    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Highest frame rate of the map canvas and the 3D view.
    var maxFrameRate: Float { 120 }
    /// The breathing halo and other idle animations.
    var animationsEnabled: Bool { true }
    /// "Keep screen awake" may hold the screen on.
    var allowsKeepScreenAwake: Bool { true }
    /// Finest terrain zoom drawn (off-map contours, world view, 3D).
    var maxTerrainZoom: Int { 13 }
    /// Background terrain prefetching (trailhead caches wait for the saver to end).
    var allowsPrefetch: Bool { true }
}

@MainActor
enum BatteryFeature {
    static func attach(_ model: AppModel) {
        model.battery.attach(model)
    }

    /// Debug launch arguments: `-mockLowPowerMode`.
    static func appDidLaunch(_ model: AppModel) async {}
}
