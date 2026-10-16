import Foundation

/// The sun's times for one day at one place, computed on the device (NOAA solar calculator, no
/// network): sunrise and sunset at −0.833° (refraction and the sun's radius), civil twilight at
/// −6°. Nil where the sun does not cross that altitude that day (polar day or night:
/// `isPolarDay` / `isPolarNight` say which).
struct SunTimes: Equatable, Sendable {
    var sunrise: Date?
    var sunset: Date?
    var civilDawn: Date?
    var civilDusk: Date?
    var isPolarDay = false
    var isPolarNight = false

    /// The times on the calendar day of `date` in `timeZone` at `position`.
    static func compute(on date: Date, at position: LatLon, timeZone: TimeZone = .current) -> SunTimes {
        // Filled in by the routes feature (NOAA algorithm; docs/design/explorer.md, Daylight).
        SunTimes()
    }

    /// Daylight left at `now` (until sunset; 0 after it; nil when unknown or polar).
    func daylightLeft(at now: Date) -> TimeInterval? {
        guard let sunset else { return nil }
        return max(0, sunset.timeIntervalSince(now))
    }
}

/// Walking time from a profile: Tobler's hiking function, scaled by your pace (docs/design/
/// explorer.md, Walking time).
enum PaceModel {
    /// Tobler's hiking function: walking speed (m/s) on a slope (rise over run):
    /// 6 · e^(−3.5 · |slope + 0.05|) km/h, 5.04 km/h on the flat, fastest slightly downhill.
    static func toblerSpeed(slope: Double) -> Double {
        guard slope.isFinite else { return 0 }
        return 6 * exp(-3.5 * abs(slope + 0.05)) / 3.6
    }

    /// Moving time along a profile at `factor` × Tobler's time (1 = the model, 1.2 = 20 %
    /// slower), from `fromDistanceM` to the end. Nil when the profile has no elevations.
    static func movingTime(along profile: ElevationProfile, factor: Double, fromDistanceM: Double = 0) -> TimeInterval? {
        // Filled in by the routes feature.
        nil
    }

    /// Your pace factor from recorded tracks: the median of (moving time ÷ Tobler's time) over the
    /// last tracks with at least 1 km of moving, clamped to 0.6…2. Nil without such tracks.
    static func learnedFactor(from tracks: [Track]) -> Double? {
        // Filled in by the routes feature.
        nil
    }
}
