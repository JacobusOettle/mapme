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

    /// Sun altitude of sunrise and sunset (degrees): the upper limb on a sea-level horizon with
    /// standard refraction.
    static let sunriseAltitude = -0.833
    /// Sun altitude at civil dawn and dusk (degrees): too dark to walk without a light after it.
    static let civilAltitude = -6.0

    /// The times on the calendar day of `date` in `timeZone` at `position`.
    ///
    /// NOAA's solar calculator (Meeus' low-precision solar coordinates and equation of time): each
    /// event is first placed from the sun's position at solar noon, then once more from the sun's
    /// position at that first estimate. Against PyEphem (VSOP87) this lands within 5 s in Pretoria,
    /// Quito, Fiji and Tromsø (docs/design/explorer.md, Sun).
    static func compute(on date: Date, at position: LatLon, timeZone: TimeZone = .current) -> SunTimes {
        var local = Calendar(identifier: .gregorian)
        local.timeZone = timeZone
        let day = local.dateComponents([.year, .month, .day], from: date)
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC") ?? TimeZone(secondsFromGMT: 0) ?? timeZone
        // Solar noon falls on the local calendar day when the time zone roughly follows the
        // longitude, so the events are counted from 00:00 UTC of the same year, month and day.
        guard let midnight = utc.date(from: DateComponents(year: day.year, month: day.month, day: day.day)),
              position.lat.isFinite, position.lon.isFinite
        else { return SunTimes() }
        let lat = min(max(position.lat, -89.999), 89.999)
        let lon = position.lon

        var out = SunTimes()
        let rising = event(midnight: midnight, lat: lat, lon: lon, altitude: sunriseAltitude, sign: -1)
        let setting = event(midnight: midnight, lat: lat, lon: lon, altitude: sunriseAltitude, sign: 1)
        out.sunrise = rising.date
        out.sunset = setting.date
        // The sun never reaches the altitude (cos HA > 1): polar night; never sinks to it: polar day.
        if out.sunrise == nil || out.sunset == nil {
            let state = rising.date == nil ? rising.state : setting.state
            out.isPolarDay = state == .alwaysAbove
            out.isPolarNight = state == .alwaysBelow
        }
        out.civilDawn = event(midnight: midnight, lat: lat, lon: lon, altitude: civilAltitude, sign: -1).date
        out.civilDusk = event(midnight: midnight, lat: lat, lon: lon, altitude: civilAltitude, sign: 1).date
        return out
    }

    /// The sun times that matter at `now` (the banner's and the daylight sheet's): today's, or
    /// yesterday's while yesterday's evening is not over. After midnight at high latitudes the sun
    /// can still be up, or civil dusk still to come, from the day before (Rovaniemi in early July:
    /// sunset at 00:54), and today's evening then lies a whole day ahead.
    static func around(_ now: Date, at position: LatLon, timeZone: TimeZone = .current) -> SunTimes {
        let today = compute(on: now, at: position, timeZone: timeZone)
        // Once the sun has risen today, yesterday is over.
        if let sunrise = today.sunrise, now >= sunrise { return today }
        var local = Calendar(identifier: .gregorian)
        local.timeZone = timeZone
        guard let dayBefore = local.date(byAdding: .day, value: -1, to: now) else { return today }
        let yesterday = compute(on: dayBefore, at: position, timeZone: timeZone)
        if let evening = yesterday.civilDusk ?? yesterday.sunset, now < evening { return yesterday }
        return today
    }

    /// Daylight left at `now`: until sunset, 0 after it. Nil before sunrise (the day's light is
    /// still to come: `timeToSunrise`), when unknown, or polar.
    func daylightLeft(at now: Date) -> TimeInterval? {
        guard let sunset else { return nil }
        if let sunrise, now < sunrise { return nil }
        return max(0, sunset.timeIntervalSince(now))
    }

    /// Time until sunrise while `now` is before it (an alpine start); nil otherwise.
    func timeToSunrise(at now: Date) -> TimeInterval? {
        guard let sunrise, now < sunrise else { return nil }
        return sunrise.timeIntervalSince(now)
    }

    // MARK: - NOAA solar calculator

    private enum Crossing: Equatable {
        case crosses
        case alwaysAbove
        case alwaysBelow
    }

    /// One crossing of `altitude`: rising (`sign` −1) or setting (+1).
    private static func event(midnight: Date, lat: Double, lon: Double, altitude: Double,
                              sign: Double) -> (date: Date?, state: Crossing) {
        // Start from solar noon at Greenwich time, then refine twice.
        var minutes = 720 - 4 * lon
        for _ in 0..<2 {
            let t = midnight.addingTimeInterval(minutes * 60)
            let sun = solarPosition(at: t)
            let noon = 720 - 4 * lon - sun.equationOfTimeMinutes
            let phi: Double = lat * .pi / 180
            let h0: Double = altitude * .pi / 180
            let numerator: Double = sin(h0) - sin(phi) * sin(sun.declination)
            let denominator: Double = cos(phi) * cos(sun.declination)
            let cosHA = numerator / denominator
            if cosHA > 1 { return (nil, .alwaysBelow) }
            if cosHA < -1 { return (nil, .alwaysAbove) }
            let hourAngle = acos(cosHA) * 180 / .pi
            minutes = noon + sign * 4 * hourAngle
        }
        return (midnight.addingTimeInterval(minutes * 60), .crosses)
    }

    /// The sun's declination (radians) and the equation of time (minutes) at `date`.
    private static func solarPosition(at date: Date) -> (declination: Double, equationOfTimeMinutes: Double) {
        // Kept in small typed steps: long mixed expressions are slow for the type checker.
        let julianDay: Double = date.timeIntervalSince1970 / 86_400 + 2_440_587.5
        let t: Double = (julianDay - 2_451_545) / 36_525
        let deg: Double = Double.pi / 180
        // Geometric mean longitude and anomaly of the sun, eccentricity of Earth's orbit.
        let l0Raw: Double = 280.46646 + t * (36_000.76983 + t * 0.0003032)
        var l0: Double = l0Raw.truncatingRemainder(dividingBy: 360)
        if l0 < 0 { l0 += 360 }
        let m: Double = 357.52911 + t * (35_999.05029 - 0.0001537 * t)
        let e: Double = 0.016708634 - t * (0.000042037 + 0.0000001267 * t)
        let mr: Double = m * deg
        // Equation of centre, true and apparent longitude.
        let c1: Double = sin(mr) * (1.914602 - t * (0.004817 + 0.000014 * t))
        let c2: Double = sin(2 * mr) * (0.019993 - 0.000101 * t)
        let c3: Double = sin(3 * mr) * 0.000289
        let trueLongitude: Double = l0 + c1 + c2 + c3
        let omega: Double = 125.04 - 1934.136 * t
        let apparentLongitude: Double = trueLongitude - 0.00569 - 0.00478 * sin(omega * deg)
        // Obliquity of the ecliptic, declination.
        let arcSeconds: Double = 21.448 - t * (46.815 + t * (0.00059 - t * 0.001813))
        let meanObliquity: Double = 23 + (26 + arcSeconds / 60) / 60
        let obliquity: Double = meanObliquity + 0.00256 * cos(omega * deg)
        let declination: Double = asin(sin(obliquity * deg) * sin(apparentLongitude * deg))
        // Equation of time (radians, then minutes).
        let y: Double = pow(tan(obliquity * deg / 2), 2)
        let l0r: Double = l0 * deg
        let q1: Double = y * sin(2 * l0r)
        let q2: Double = 2 * e * sin(mr)
        let q3: Double = 4 * e * y * sin(mr) * cos(2 * l0r)
        let q4: Double = 0.5 * y * y * sin(4 * l0r)
        let q5: Double = 1.25 * e * e * sin(2 * mr)
        let eq: Double = q1 - q2 + q3 - q4 - q5
        return (declination, 4 * eq / deg)
    }
}

/// The clock the daylight banner and ETAs read: the real one, or in Debug builds a fixed start
/// (`-mockNow 2026-06-21T15:30:00+02:00`) that then runs on, for UI tests and screenshots.
enum RoutesClock {
    /// Added to the real clock; worked out at first use.
    static let offset: TimeInterval = {
        #if DEBUG
        if let text = AppSettings.launchValue("-mockNow", in: ProcessInfo.processInfo.arguments),
           let date = ISO8601DateFormatter().date(from: text) {
            return date.timeIntervalSinceNow
        }
        #endif
        return 0
    }()

    static func now() -> Date { Date().addingTimeInterval(offset) }
}

/// How the walking pace in use was found, for "at your pace" (docs/design/explorer.md, Walking time).
struct PaceEstimate: Equatable, Sendable {
    enum Source: Equatable, Sendable {
        /// Tobler's function as it is: no recorded tracks yet.
        case model
        /// The median of your recorded tracks (how many).
        case learned(tracks: Int)
        /// Set in Settings → Routes.
        case custom
    }

    /// × Tobler's walking time (1.2 = 20 % slower than the model).
    var factor: Double
    var source: Source

    static let model = PaceEstimate(factor: 1, source: .model)

    /// "1.12× Tobler, learned from 4 tracks".
    var description: String {
        let f = String(format: "%.2f× Tobler", factor)
        switch source {
        case .model: return "Tobler's hiking function (no recorded tracks yet)"
        case .learned(let n): return "\(f), learned from \(n) track\(n == 1 ? "" : "s")"
        case .custom: return "\(f), set in Settings"
        }
    }

    /// Walking speed on the flat at this pace, m/s.
    var flatSpeed: Double { PaceModel.toblerSpeed(slope: 0) / max(factor, 0.01) }
}

/// What one stretch of recorded walking says about your pace.
struct WalkPace: Equatable, Sendable {
    /// Seconds spent moving (0.2–3 m/s, no stops over 2 min).
    var movingSeconds: Double
    /// Tobler's time for the same moving stretches.
    var toblerSeconds: Double
    /// Horizontal metres moved.
    var movingMeters: Double

    /// Moving time ÷ Tobler's time (nil without moving).
    var factor: Double? {
        guard movingSeconds > 0, toblerSeconds > 0 else { return nil }
        return movingSeconds / toblerSeconds
    }
}

/// Walking time from a profile: Tobler's hiking function, scaled by your pace (docs/design/
/// explorer.md, Walking time).
enum PaceModel {
    /// Pace factors are kept within this: anything outside is a GPS problem or a bike ride.
    static let factorRange: ClosedRange<Double> = 0.6...2.0
    /// Tracks with less moving than this say little about a pace.
    static let minTrackMovingMeters = 1000.0
    /// The recorded tracks the learned pace looks at (newest first).
    static let learnedTrackCount = 10
    /// Moving is between these speeds (m/s): slower is standing, faster a GPS jump or a vehicle.
    static let movingSpeedRange: ClosedRange<Double> = 0.2...3.0
    /// A gap between fixes longer than this is a stop and left out.
    static let maxMovingGap: TimeInterval = 120
    /// GPS altitude is noisy: slopes of recorded tracks are taken over at least this distance.
    static let slopeChunkMeters = 100.0
    /// The live pace of the walk you are on counts over this window…
    static let livePaceWindow: TimeInterval = 45 * 60
    /// …with at most this weight against the learned one, reached after a full window of moving.
    static let livePaceMaxWeight = 0.5

    /// Tobler's hiking function: walking speed (m/s) on a slope (rise over run):
    /// 6 · e^(−3.5 · |slope + 0.05|) km/h, 5.04 km/h on the flat, fastest slightly downhill.
    static func toblerSpeed(slope: Double) -> Double {
        guard slope.isFinite else { return 0 }
        return 6 * exp(-3.5 * abs(slope + 0.05)) / 3.6
    }

    /// Tobler's time (s) for `meters` on a constant `slope`.
    static func toblerTime(meters: Double, slope: Double) -> Double {
        let v = toblerSpeed(slope: slope)
        guard meters > 0, v > 0 else { return 0 }
        return meters / v
    }

    /// Naismith's rule, the hill walkers' other rule of thumb: 5 km/h on the map plus an hour per
    /// 600 m of ascent. Shown next to Tobler's time as a second opinion.
    static func naismithTime(distanceM: Double, ascentM: Double) -> TimeInterval {
        let walking: TimeInterval = max(0, distanceM) / (5000.0 / 3600)
        let climbing: TimeInterval = max(0, ascentM) / 600 * 3600
        return walking + climbing
    }

    /// Moving time along a profile at `factor` × Tobler's time (1 = the model, 1.2 = 20 %
    /// slower), from `fromDistanceM` to the end. Nil when the profile has no elevations.
    /// Stretches without an elevation count as flat.
    static func movingTime(along profile: ElevationProfile, factor: Double, fromDistanceM: Double = 0) -> TimeInterval? {
        let samples = profile.samples
        guard samples.contains(where: { $0.elevationM != nil }) else { return nil }
        let start = max(0, fromDistanceM)
        var seconds = 0.0
        if samples.count >= 2 {
            for i in 1..<samples.count {
                let a = samples[i - 1], b = samples[i]
                let d0 = max(a.distanceM, start)
                let d1 = b.distanceM
                guard d1 > d0, b.distanceM > a.distanceM else { continue }
                var slope = 0.0
                if let e0 = a.elevationM, let e1 = b.elevationM {
                    slope = (e1 - e0) / (b.distanceM - a.distanceM)
                }
                seconds += toblerTime(meters: d1 - d0, slope: slope)
            }
        }
        // The samples may stop short of the line's length (a coarse profile): the rest is flat.
        if let last = samples.last, profile.distanceM > max(last.distanceM, start) {
            seconds += toblerTime(meters: profile.distanceM - max(last.distanceM, start), slope: 0)
        }
        return seconds * max(factor, 0.01)
    }

    /// Moving time over `meters` of flat ground at `factor` (no terrain known).
    static func flatTime(meters: Double, factor: Double) -> TimeInterval {
        toblerTime(meters: max(0, meters), slope: 0) * max(factor, 0.01)
    }

    /// Moving time and Tobler's time of a recorded walk: moving stretches only (0.2–3 m/s, gaps
    /// over 2 min left out), slopes from the GPS altitude over ≥ 100 m chunks.
    static func walkPace(of points: [TrackPoint]) -> WalkPace {
        var pace = WalkPace(movingSeconds: 0, toblerSeconds: 0, movingMeters: 0)
        guard points.count >= 2 else { return pace }
        // The chunk being collected: its horizontal metres, seconds and the altitudes at its ends.
        var chunkMeters = 0.0
        var chunkSeconds = 0.0
        var chunkStartAltitude: Double?
        var chunkEndAltitude: Double?

        func closeChunk() {
            guard chunkMeters > 0 else { return }
            var slope = 0.0
            if let a = chunkStartAltitude, let b = chunkEndAltitude {
                slope = min(max((b - a) / chunkMeters, -1), 1)
            }
            pace.movingSeconds += chunkSeconds
            pace.movingMeters += chunkMeters
            pace.toblerSeconds += toblerTime(meters: chunkMeters, slope: slope)
            chunkMeters = 0
            chunkSeconds = 0
            chunkStartAltitude = nil
            chunkEndAltitude = nil
        }

        for i in 1..<points.count {
            let a = points[i - 1], b = points[i]
            let dt = b.timestamp.timeIntervalSince(a.timestamp)
            let d = Geodesy.distance(a.latLon, b.latLon)
            guard dt > 0, dt <= maxMovingGap, movingSpeedRange.contains(d / dt) else {
                // A stop or a jump ends the chunk.
                closeChunk()
                continue
            }
            if chunkMeters == 0 { chunkStartAltitude = a.altitude }
            chunkMeters += d
            chunkSeconds += dt
            chunkEndAltitude = b.altitude
            if chunkMeters >= slopeChunkMeters { closeChunk() }
        }
        closeChunk()
        return pace
    }

    /// Your pace factor from recorded tracks: the median of (moving time ÷ Tobler's time) over the
    /// last tracks with at least 1 km of moving, clamped to 0.6…2. Nil without such tracks.
    static func learnedFactor(from tracks: [Track]) -> Double? {
        learnedPace(from: tracks)?.factor
    }

    /// `learnedFactor` with the number of tracks it came from.
    static func learnedPace(from tracks: [Track]) -> PaceEstimate? {
        var factors: [Double] = []
        for track in tracks.sorted(by: { $0.startedAt > $1.startedAt }) {
            let pace = walkPace(of: track.points)
            guard pace.movingMeters >= minTrackMovingMeters, let f = pace.factor, f.isFinite else { continue }
            factors.append(f)
            if factors.count == learnedTrackCount { break }
        }
        guard !factors.isEmpty else { return nil }
        factors.sort()
        let n = factors.count
        let median = n % 2 == 1 ? factors[n / 2] : (factors[n / 2 - 1] + factors[n / 2]) / 2
        return PaceEstimate(factor: clamp(median), source: .learned(tracks: n))
    }

    /// `base` with the pace of the walk you are on blended in: the last 45 min of moving count up
    /// to half the weight (a quarter after 22 min of moving, and so on).
    static func blended(base: Double, recent points: [TrackPoint], now: Date) -> Double {
        let since = now.addingTimeInterval(-livePaceWindow)
        guard let first = points.firstIndex(where: { $0.timestamp >= since }) else { return base }
        let pace = walkPace(of: Array(points[first...]))
        // Five minutes and 300 m of moving before the live pace counts at all.
        guard pace.movingSeconds >= 300, pace.movingMeters >= 300, let live = pace.factor, live.isFinite else { return base }
        let weight = livePaceMaxWeight * min(1, pace.movingSeconds / livePaceWindow)
        return clamp((1 - weight) * base + weight * clamp(live))
    }

    static func clamp(_ factor: Double) -> Double {
        min(max(factor, factorRange.lowerBound), factorRange.upperBound)
    }
}

/// The daylight banner's words and colour (docs/design/explorer.md, Daylight banner): pure, so
/// the rules are tested without a screen.
struct DaylightForecast: Equatable {
    enum Level: Equatable {
        /// Plenty of time.
        case normal
        /// The ETA is within 15 min of sunset (or after it, before dark), or in the morning
        /// twilight; without a route: the last 15 min of the day, the evening twilight, the dark
        /// before dawn.
        case amber
        /// You arrive after civil dusk, or before civil dawn.
        case red
    }

    var text: String
    var level: Level
    /// Seconds of daylight left (nil: polar, unknown, or before sunrise).
    var daylightLeft: TimeInterval?

    /// Amber from this long before sunset.
    static let amberMargin: TimeInterval = 15 * 60
    /// Without a route the banner shows from this long before sunset.
    static let showBefore: TimeInterval = 2 * 3600

    /// The banner text at `now`, from the sun times around it (`SunTimes.around`). `eta` and
    /// `destination` while following ("Bannerman Hut", "the end of Bannerman Pass"); nil `eta`
    /// without one. Nil when nothing needs saying (more than two hours of daylight and no route;
    /// after dark without a route, until two hours before first light).
    static func make(now: Date, sun: SunTimes, eta: Date?, destination: String?,
                     time: (Date) -> String = { $0.formatted(date: .omitted, time: .shortened) }) -> DaylightForecast? {
        let following = eta != nil && destination != nil
        if sun.isPolarDay {
            guard following, let eta, let destination else { return nil }
            return DaylightForecast(text: "The sun doesn't set today · at your pace you reach \(destination) at \(time(eta))",
                                    level: .normal, daylightLeft: nil)
        }
        if sun.isPolarNight {
            guard following, let eta, let destination else {
                return DaylightForecast(text: "The sun doesn't rise today", level: .normal, daylightLeft: nil)
            }
            let dark = sun.civilDusk.map { eta > $0 } ?? true
            return DaylightForecast(text: "The sun doesn't rise today · you reach \(destination) at \(time(eta))",
                                    level: dark ? .red : .amber, daylightLeft: nil)
        }
        guard let sunset = sun.sunset else { return nil }
        if let sunrise = sun.sunrise, now < sunrise {
            return beforeSunrise(now: now, sun: sun, sunrise: sunrise, sunset: sunset, eta: eta,
                                 destination: destination, time: time)
        }
        let left = max(0, sunset.timeIntervalSince(now))
        let dusk = sun.civilDusk
        if !following {
            // Only in the last two hours of the day, until dark.
            guard left < showBefore else { return nil }
            if left > 0 {
                return DaylightForecast(text: "\(RouteFormat.duration(left)) of daylight left · sunset \(time(sunset))",
                                        level: left <= amberMargin ? .amber : .normal, daylightLeft: left)
            }
            guard let dusk, now < dusk else { return nil }
            return DaylightForecast(text: "The sun has set · dark at \(time(dusk))", level: .amber, daylightLeft: 0)
        }
        guard let eta, let destination else { return nil }
        let head = left > 0 ? "\(RouteFormat.duration(left)) of daylight left" : "The sun has set"
        if let dusk, eta > dusk {
            let late = eta.timeIntervalSince(dusk)
            return DaylightForecast(text: "\(head) · you reach \(destination) \(RouteFormat.duration(late)) after dark",
                                    level: .red, daylightLeft: left)
        }
        let level: Level = eta > sunset.addingTimeInterval(-amberMargin) ? .amber : .normal
        return DaylightForecast(text: "\(head) · at your pace you reach \(destination) at \(time(eta))",
                                level: level, daylightLeft: left)
    }

    /// Before sunrise (an alpine start): "Dark until 06:31 · sunrise 06:56" until civil dawn (a
    /// light is needed), "Sunrise at 06:56" in the morning twilight. While following: red when you
    /// arrive before first light, amber in the twilight, and the day's rules for an arrival late in
    /// the day. Without a route: from two hours before first light.
    private static func beforeSunrise(now: Date, sun: SunTimes, sunrise: Date, sunset: Date, eta: Date?,
                                      destination: String?, time: (Date) -> String) -> DaylightForecast? {
        // Nil once civil dawn is past, or on a night that never gets that dark (white nights).
        let darkUntil: Date? = sun.civilDawn.flatMap { now < $0 ? $0 : nil }
        let head: String
        if let darkUntil {
            head = "Dark until \(time(darkUntil)) · sunrise \(time(sunrise))"
        } else {
            head = "Sunrise at \(time(sunrise))"
        }
        guard let eta, let destination else {
            let firstLight = darkUntil ?? sunrise
            guard firstLight.timeIntervalSince(now) < showBefore else { return nil }
            return DaylightForecast(text: head, level: darkUntil == nil ? .normal : .amber, daylightLeft: nil)
        }
        if let dusk = sun.civilDusk, eta > dusk {
            let late = eta.timeIntervalSince(dusk)
            return DaylightForecast(text: "\(head) · you reach \(destination) \(RouteFormat.duration(late)) after dark",
                                    level: .red, daylightLeft: nil)
        }
        if let dawn = sun.civilDawn, eta < dawn {
            return DaylightForecast(text: "\(head) · you reach \(destination) at \(time(eta)), before first light",
                                    level: .red, daylightLeft: nil)
        }
        let level: Level = eta < sunrise || eta > sunset.addingTimeInterval(-amberMargin) ? .amber : .normal
        return DaylightForecast(text: "\(head) · at your pace you reach \(destination) at \(time(eta))",
                                level: level, daylightLeft: nil)
    }
}

/// Wording of route numbers.
enum RouteFormat {
    /// "2 h 10 min", "45 min", "under a minute".
    static func duration(_ seconds: TimeInterval) -> String {
        guard seconds.isFinite else { return "—" }
        let minutes = Int((max(0, seconds) / 60).rounded())
        if minutes < 1 { return "under a minute" }
        if minutes < 60 { return "\(minutes) min" }
        let h = minutes / 60, m = minutes % 60
        return m == 0 ? "\(h) h" : "\(h) h \(m) min"
    }

    /// "2:10 h" style, short, for stat tiles ("0:45").
    static func shortDuration(_ seconds: TimeInterval) -> String {
        guard seconds.isFinite else { return "—" }
        let minutes = Int((max(0, seconds) / 60).rounded())
        return String(format: "%d:%02d", minutes / 60, minutes % 60)
    }
}
