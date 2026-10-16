import CoreLocation
import Foundation
import Observation
import UIKit

/// CoreLocation settings for a power profile (applied by `LocationService.apply(_:)`).
struct LocationPowerProfile: Equatable, Sendable {
    var desiredAccuracy: CLLocationAccuracy
    var distanceFilter: CLLocationDistance
    var headingFilter: CLLocationDegrees
    /// Compass updates run. Only the heading cone on the map uses them.
    var headingUpdates: Bool = true

    /// As MapMe always ran: best accuracy, every fix, 1° heading steps.
    static let normal = LocationPowerProfile(desiredAccuracy: kCLLocationAccuracyBest,
                                             distanceFilter: kCLDistanceFilterNone, headingFilter: 1)
}

/// What the rest of the app may do while the saver is on or off (the map canvas, the 3D view, the
/// terrain features read these through `BatterySaver`).
struct BatteryLimits: Equatable, Sendable {
    /// Highest redraw rate of the map canvas and the 3D view (the 3D view caps it at 60).
    var maxFrameRate: Float
    /// The breathing puck and other idle animations.
    var animationsEnabled: Bool
    /// "Keep screen awake" may hold the screen on.
    var allowsKeepScreenAwake: Bool
    /// Finest terrain zoom drawn (off-map contours, world view, 3D).
    var maxTerrainZoom: Int
    /// Background terrain prefetching and trailhead caching.
    var allowsPrefetch: Bool
    /// Multisampling in the 3D view (4× when allowed).
    var allowsMultisampling: Bool

    static let normal = BatteryLimits(maxFrameRate: 120, animationsEnabled: true, allowsKeepScreenAwake: true,
                                      maxTerrainZoom: 13, allowsPrefetch: true, allowsMultisampling: true)
    static let saver = BatteryLimits(maxFrameRate: 30, animationsEnabled: false, allowsKeepScreenAwake: false,
                                     maxTerrainZoom: 12, allowsPrefetch: false, allowsMultisampling: false)
}

/// The battery saver (docs/design/explorer.md, Battery saver): what it changes is exactly what
/// iOS lets an app change (location accuracy and filters, heading updates, redraw rate,
/// animations, keeping the screen awake, background work), on with Low Power Mode by default.
/// Use from the main thread.
///
/// What it does not do, because iOS does not let an app: duty-cycle the GPS (deferred updates are
/// gone since iOS 13), dim or turn off the screen (`UIScreen.brightness` only lasts while the app
/// is in front and is put back by iOS), or change the display's refresh rate (Low Power Mode
/// already caps ProMotion at 60 Hz). It never lets iOS pause location updates by itself: that
/// would cut a recorded track or a followed route.
@MainActor
@Observable
final class BatterySaver {
    /// The saver is on now (the setting, or Low Power Mode with the setting on "auto").
    private(set) var isActive = false
    /// Low Power Mode as last seen (`-mockLowPowerMode` in Debug builds says it is on).
    private(set) var isLowPowerMode = false
    /// MapMe is in the background (only a recording or a followed route keeps it running there).
    private(set) var isInBackground = false
    /// This iPhone's battery drain measured this session while unplugged, once there is enough to
    /// say (≥ 15 min and ≥ 2 %).
    private(set) var measuredDrain: BatteryDrainMeter.Rate?

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored private var mockLowPowerMode = false
    @ObservationIgnored private var observers: [NSObjectProtocol] = []
    @ObservationIgnored private var loop: ObservationLoop?
    /// What `evaluate` last worked out (the profile is applied from these without observing).
    @ObservationIgnored private var active = false
    @ObservationIgnored private var mapOnScreen = false
    @ObservationIgnored private var background = false
    /// The profile last given to the location service.
    @ObservationIgnored private var appliedProfile: LocationPowerProfile?
    /// Let the next fix through whatever the distance filter (see `heartbeatInterval`).
    @ObservationIgnored private var wantsFreshFix = true
    @ObservationIgnored private var heartbeatTask: Task<Void, Never>?
    @ObservationIgnored private var meter = BatteryDrainMeter()
    @ObservationIgnored private var samplingTask: Task<Void, Never>?
    @ObservationIgnored private var monitoringBattery = false

    /// With a distance filter a phone standing still gets no fixes, and after
    /// `MapOverlayView.staleAfter` (30 s) the puck would turn grey as if the GPS were lost. So
    /// after this long without a fix on screen the filter opens for one fix: the puck stays live
    /// while you stand, and still greys honestly when the GPS really has nothing.
    static let heartbeatInterval: TimeInterval = 20
    /// Battery level sampling while unplugged (also on every level change iOS reports).
    static let samplingInterval: TimeInterval = 5 * 60

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
        mockLowPowerMode = Self.mocksLowPowerMode(ProcessInfo.processInfo.arguments)
        isLowPowerMode = mockLowPowerMode || ProcessInfo.processInfo.isLowPowerModeEnabled
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: .NSProcessInfoPowerStateDidChange, object: nil, queue: nil) { [weak self] _ in
            let on = ProcessInfo.processInfo.isLowPowerModeEnabled
            Task { @MainActor [weak self] in self?.lowPowerModeChanged(on) }
        })
        observers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil,
                                            queue: .main) { [weak self] _ in
            Task { @MainActor [weak self] in self?.setInBackground(true) }
        })
        observers.append(center.addObserver(forName: UIApplication.willEnterForegroundNotification, object: nil,
                                            queue: .main) { [weak self] _ in
            Task { @MainActor [weak self] in self?.setInBackground(false) }
        })
        model.location.addLocationObserver { [weak self] _ in
            self?.fixArrived()
        }
        loop = ObservationLoop { [weak self] in self?.evaluate() }
    }

    // MARK: - What the app may do

    var limits: BatteryLimits { isActive ? .saver : .normal }
    /// Highest frame rate of the map canvas and the 3D view.
    var maxFrameRate: Float { limits.maxFrameRate }
    /// The breathing halo and other idle animations.
    var animationsEnabled: Bool { limits.animationsEnabled }
    /// "Keep screen awake" may hold the screen on.
    var allowsKeepScreenAwake: Bool { limits.allowsKeepScreenAwake }
    /// Finest terrain zoom drawn (off-map contours, world view, 3D).
    var maxTerrainZoom: Int { limits.maxTerrainZoom }
    /// Background terrain prefetching (trailhead caches wait for the saver to end).
    var allowsPrefetch: Bool { limits.allowsPrefetch }
    /// 4× multisampling in the 3D view.
    var allowsMultisampling: Bool { limits.allowsMultisampling }

    /// "On · Low Power Mode", "Off", … for Settings and the badge.
    var statusText: String {
        let mode = model?.settings.batterySaver ?? .auto
        switch (isActive, mode) {
        case (true, .auto): return "On · Low Power Mode is on"
        case (true, _): return "On"
        case (false, .auto): return "Off · on with Low Power Mode"
        case (false, _): return "Off"
        }
    }

    /// "−6 % per hour · measured over 40 min", once measured.
    var drainText: String? {
        guard let rate = measuredDrain else { return nil }
        let percent = max(0, rate.perHour * 100)
        let value = percent < 10 ? String(format: "%.1f", percent) : String(format: "%.0f", percent)
        return "\u{2212}\(value) % per hour · measured over \(rate.minutes) min"
    }

    // MARK: - Rules

    /// The saver is on: always, or with Low Power Mode.
    nonisolated static func isActive(mode: BatterySaverMode, lowPowerMode: Bool) -> Bool {
        switch mode {
        case .off: return false
        case .auto: return lowPowerMode
        case .on: return true
        }
    }

    /// CoreLocation's settings (docs/design/explorer.md, Battery saver):
    ///
    /// | | normal | saver |
    /// |---|---|---|
    /// | accuracy | best | nearest ten metres (still the GPS; coarser turns it off and fails without Wi-Fi or cell) |
    /// | distance filter | none | 5 m on screen, 10 m in the background (recording, following) |
    /// | heading | 1° steps | 5° steps, off in the background and while no map is on screen |
    ///
    /// `freshFix` opens the distance filter for one fix (after 20 s without one, and on coming
    /// back to the front).
    nonisolated static func profile(saverActive: Bool, inBackground: Bool, mapOnScreen: Bool,
                                    freshFix: Bool) -> LocationPowerProfile {
        guard saverActive else { return .normal }
        if inBackground {
            return LocationPowerProfile(desiredAccuracy: kCLLocationAccuracyNearestTenMeters, distanceFilter: 10,
                                        headingFilter: 5, headingUpdates: false)
        }
        return LocationPowerProfile(desiredAccuracy: kCLLocationAccuracyNearestTenMeters,
                                    distanceFilter: freshFix ? kCLDistanceFilterNone : 5,
                                    headingFilter: 5, headingUpdates: mapOnScreen)
    }

    /// `-mockLowPowerMode` (Debug builds): the saver sees Low Power Mode on.
    nonisolated static func mocksLowPowerMode(_ arguments: [String]) -> Bool {
        #if DEBUG
        return arguments.contains("-mockLowPowerMode")
        #else
        return false
        #endif
    }

    /// What it means, for Settings and the badge (docs/design/explorer.md, Battery saver: the
    /// honest estimate).
    static let honestEstimate = """
        With the screen on, the display and the GPS use most of the power. The saver's slower redraw, \
        still map and fewer location updates typically give 10–25 % more time with MapMe open, and \
        letting the screen lock is the biggest saving by itself. With the screen off while recording, \
        the GPS dominates and the saver changes little (up to 10 %). Your track keeps recording either way.
        """

    // MARK: - Internals

    /// Works out whether the saver is on and applies it (observed: runs again when the setting,
    /// Low Power Mode, the app's state or the screen shown change).
    private func evaluate() {
        guard let model else { return }
        let nowActive = Self.isActive(mode: model.settings.batterySaver, lowPowerMode: isLowPowerMode)
        var onMap = false
        if case .map = model.path.last { onMap = true }
        let inBackground = isInBackground
        if nowActive != active {
            active = nowActive
            isActive = nowActive
            // A filter just closing must not leave the puck waiting for you to walk 5 m.
            wantsFreshFix = true
        }
        mapOnScreen = onMap
        background = inBackground
        applyProfile()
        scheduleHeartbeat()
    }

    private func applyProfile() {
        guard let model else { return }
        let profile = Self.profile(saverActive: active, inBackground: background, mapOnScreen: mapOnScreen,
                                   freshFix: wantsFreshFix)
        guard profile != appliedProfile else { return }
        appliedProfile = profile
        model.location.apply(profile)
    }

    private func lowPowerModeChanged(_ on: Bool) {
        let value = mockLowPowerMode || on
        if value != isLowPowerMode { isLowPowerMode = value }
    }

    private func setInBackground(_ value: Bool) {
        guard value != isInBackground else { return }
        if !value {
            // Back in front: the first fix comes through at once, so the puck is current.
            wantsFreshFix = true
        }
        isInBackground = value
        if value {
            stopSampling()
        } else {
            startSampling()
        }
    }

    private func fixArrived() {
        if wantsFreshFix {
            wantsFreshFix = false
            applyProfile()
        }
        scheduleHeartbeat()
    }

    /// While the saver filters fixes on screen, lets one through after `heartbeatInterval`
    /// without any.
    private func scheduleHeartbeat() {
        heartbeatTask?.cancel()
        heartbeatTask = nil
        guard active, !background else { return }
        heartbeatTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.heartbeatInterval * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            self.wantsFreshFix = true
            self.applyProfile()
        }
    }

    // MARK: - Measured drain

    /// Starts measuring the battery (once the UI is up; not while hosting unit tests).
    func startMonitoring() {
        guard !monitoringBattery, !LocationService.isHostingUnitTests else { return }
        monitoringBattery = true
        let device = UIDevice.current
        device.isBatteryMonitoringEnabled = true
        let center = NotificationCenter.default
        for name in [UIDevice.batteryLevelDidChangeNotification, UIDevice.batteryStateDidChangeNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor [weak self] in self?.sampleBattery() }
            })
        }
        startSampling()
    }

    private func startSampling() {
        guard monitoringBattery, samplingTask == nil else { return }
        sampleBattery()
        samplingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.samplingInterval * 1_000_000_000))
                guard !Task.isCancelled, let self else { return }
                self.sampleBattery()
            }
        }
    }

    private func stopSampling() {
        samplingTask?.cancel()
        samplingTask = nil
    }

    private func sampleBattery() {
        guard monitoringBattery else { return }
        let device = UIDevice.current
        meter.record(level: Double(device.batteryLevel), unplugged: device.batteryState == .unplugged, at: Date())
        let rate = meter.rate
        if rate != measuredDrain { measuredDrain = rate }
    }
}

/// The battery's drain over this session, from the levels iOS reports (in steps, not
/// continuously). Between two level changes the drop is exact, so the rate is taken from the first
/// change to the last one when they are far enough apart; before that, from the first sample to
/// the latest. Plugging in starts over. It is the whole iPhone's drain, not MapMe's alone: iOS
/// does not tell an app its own share.
struct BatteryDrainMeter: Equatable, Sendable {
    struct Sample: Equatable, Sendable {
        var date: Date
        var level: Double
    }

    struct Rate: Equatable, Sendable {
        /// Fraction of a full battery per hour (0.06: 6 % per hour).
        var perHour: Double
        /// Over how long it was measured, minutes.
        var minutes: Int
    }

    /// Shown after this long…
    static let minimumSpan: TimeInterval = 15 * 60
    /// …and this much drop (2 %).
    static let minimumDrop = 0.02

    private(set) var first: Sample?
    private(set) var latest: Sample?
    private(set) var firstChange: Sample?
    private(set) var lastChange: Sample?

    /// - Parameter level: 0…1 (`UIDevice.batteryLevel`; −1 when unknown, as in the Simulator).
    mutating func record(level: Double, unplugged: Bool, at date: Date) {
        guard unplugged else {
            // Charging or full: what was measured no longer says anything.
            self = BatteryDrainMeter()
            return
        }
        guard level.isFinite, level >= 0, level <= 1 else { return }
        let sample = Sample(date: date, level: level)
        guard let latest else {
            first = sample
            self.latest = sample
            return
        }
        if level > latest.level + 0.0001 {
            // Charged in between without us seeing it: start over from here.
            self = BatteryDrainMeter()
            first = sample
            self.latest = sample
            return
        }
        if level < latest.level - 0.0001 {
            if firstChange == nil { firstChange = sample }
            lastChange = sample
        }
        self.latest = sample
    }

    var rate: Rate? {
        if let a = firstChange, let b = lastChange, let r = Self.rate(from: a, to: b) { return r }
        if let a = first, let b = latest, let r = Self.rate(from: a, to: b) { return r }
        return nil
    }

    private static func rate(from a: Sample, to b: Sample) -> Rate? {
        let span = b.date.timeIntervalSince(a.date)
        let drop = a.level - b.level
        guard span >= minimumSpan, drop >= minimumDrop - 1e-9 else { return nil }
        return Rate(perHour: drop / (span / 3600), minutes: Int((span / 60).rounded()))
    }
}

@MainActor
enum BatteryFeature {
    static func attach(_ model: AppModel) {
        model.battery.attach(model)
    }

    /// Debug launch arguments: `-mockLowPowerMode` (read in `attach`). Starts measuring the
    /// battery's drain for the badge and Settings.
    static func appDidLaunch(_ model: AppModel) async {
        model.battery.startMonitoring()
    }
}
