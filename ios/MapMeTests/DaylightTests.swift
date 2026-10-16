import Foundation
import XCTest
@testable import MapMe

/// Sun times (NOAA's solar calculator, on the device) against reference values, and the daylight
/// banner's rules (docs/design/explorer.md, Sun; Daylight banner). The reference times are
/// PyEphem's (VSOP87, sun's centre at −0.833° and −6°, no refraction model beyond that), to the
/// second; the app's are allowed a minute.
final class DaylightTests: XCTestCase {
    private func zone(_ id: String) throws -> TimeZone {
        try XCTUnwrap(TimeZone(identifier: id), id)
    }

    /// A local wall-clock time in `tz`.
    private func local(_ y: Int, _ m: Int, _ d: Int, _ h: Int, _ min: Int, _ s: Int, _ tz: TimeZone) throws -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = tz
        return try XCTUnwrap(cal.date(from: DateComponents(year: y, month: m, day: d, hour: h, minute: min, second: s)))
    }

    private func assertTime(_ date: Date?, _ expected: Date, within seconds: TimeInterval = 60,
                            _ what: String, file: StaticString = #filePath, line: UInt = #line) {
        guard let date else { return XCTFail("\(what): none", file: file, line: line) }
        XCTAssertEqual(date.timeIntervalSince(expected), 0, accuracy: seconds, what, file: file, line: line)
    }

    func testPretoriaInJuneAndDecember() throws {
        let tz = try zone("Africa/Johannesburg")
        let pretoria = LatLon(lat: -25.7479, lon: 28.2293)
        let june = SunTimes.compute(on: try local(2026, 6, 21, 12, 0, 0, tz), at: pretoria, timeZone: tz)
        assertTime(june.sunrise, try local(2026, 6, 21, 6, 53, 0, tz), "June sunrise")
        assertTime(june.sunset, try local(2026, 6, 21, 17, 24, 45, tz), "June sunset")
        assertTime(june.civilDawn, try local(2026, 6, 21, 6, 27, 47, tz), "June dawn")
        assertTime(june.civilDusk, try local(2026, 6, 21, 17, 49, 58, tz), "June dusk")
        XCTAssertFalse(june.isPolarDay || june.isPolarNight)

        let december = SunTimes.compute(on: try local(2026, 12, 21, 23, 30, 0, tz), at: pretoria, timeZone: tz)
        assertTime(december.sunrise, try local(2026, 12, 21, 5, 12, 34, tz), "December sunrise")
        assertTime(december.sunset, try local(2026, 12, 21, 18, 57, 38, tz), "December sunset")
        assertTime(december.civilDawn, try local(2026, 12, 21, 4, 46, 32, tz), "December dawn")
        assertTime(december.civilDusk, try local(2026, 12, 21, 19, 23, 40, tz), "December dusk")
    }

    func testTheSampleSheetOnMidwinterDay() throws {
        // Grootkloof, where the UI tests stand (−mockNow 2026-06-21).
        let tz = try zone("Africa/Johannesburg")
        let sun = SunTimes.compute(on: try local(2026, 6, 21, 0, 5, 0, tz), at: LatLon(lat: -25.8405, lon: 27.3920), timeZone: tz)
        assertTime(sun.sunrise, try local(2026, 6, 21, 6, 56, 33, tz), "sunrise")
        assertTime(sun.sunset, try local(2026, 6, 21, 17, 27, 54, tz), "sunset")
        assertTime(sun.civilDusk, try local(2026, 6, 21, 17, 53, 8, tz), "dusk")
        let now = try local(2026, 6, 21, 15, 30, 0, tz)
        XCTAssertEqual(try XCTUnwrap(sun.daylightLeft(at: now)), 1 * 3600 + 57 * 60 + 54, accuracy: 60)
        XCTAssertEqual(sun.daylightLeft(at: try local(2026, 6, 21, 19, 0, 0, tz)), 0)
    }

    func testQuitoAndFiji() throws {
        let quitoTZ = try zone("America/Guayaquil")
        let quito = SunTimes.compute(on: try local(2026, 3, 20, 12, 0, 0, quitoTZ), at: LatLon(lat: -0.1807, lon: -78.4678),
                                     timeZone: quitoTZ)
        assertTime(quito.sunrise, try local(2026, 3, 20, 6, 17, 59, quitoTZ), "Quito sunrise")
        assertTime(quito.sunset, try local(2026, 3, 20, 18, 24, 29, quitoTZ), "Quito sunset")

        // Near the antimeridian.
        let fijiTZ = try zone("Pacific/Fiji")
        let fiji = SunTimes.compute(on: try local(2026, 1, 15, 9, 0, 0, fijiTZ), at: LatLon(lat: -17.7134, lon: 178.065),
                                    timeZone: fijiTZ)
        assertTime(fiji.sunrise, try local(2026, 1, 15, 5, 44, 36, fijiTZ), "Fiji sunrise")
        assertTime(fiji.sunset, try local(2026, 1, 15, 18, 49, 9, fijiTZ), "Fiji sunset")
    }

    func testTromsoPolarDayAndNight() throws {
        let tz = try zone("Europe/Oslo")
        let tromso = LatLon(lat: 69.6492, lon: 18.9553)
        let june = SunTimes.compute(on: try local(2026, 6, 21, 12, 0, 0, tz), at: tromso, timeZone: tz)
        XCTAssertTrue(june.isPolarDay)
        XCTAssertFalse(june.isPolarNight)
        XCTAssertNil(june.sunrise)
        XCTAssertNil(june.sunset)
        XCTAssertNil(june.civilDusk)
        XCTAssertNil(june.daylightLeft(at: Date()))

        let december = SunTimes.compute(on: try local(2026, 12, 21, 12, 0, 0, tz), at: tromso, timeZone: tz)
        XCTAssertTrue(december.isPolarNight)
        XCTAssertFalse(december.isPolarDay)
        XCTAssertNil(december.sunrise)
        // The sun stays below the horizon but rises above −6°: a few hours of twilight.
        assertTime(december.civilDawn, try local(2026, 12, 21, 9, 31, 15, tz), "Tromsø dawn")
        assertTime(december.civilDusk, try local(2026, 12, 21, 13, 53, 9, tz), "Tromsø dusk")
    }

    // MARK: - The banner

    private let time: (Date) -> String = { date in
        let f = DateFormatter()
        f.timeZone = TimeZone(identifier: "Africa/Johannesburg")
        f.dateFormat = "HH:mm"
        return f.string(from: date)
    }

    private func grootkloofSun() throws -> (SunTimes, TimeZone) {
        let tz = try zone("Africa/Johannesburg")
        let sun = SunTimes.compute(on: try local(2026, 6, 21, 12, 0, 0, tz), at: LatLon(lat: -25.8405, lon: 27.3920), timeZone: tz)
        return (sun, tz)
    }

    func testBannerWithoutARoute() throws {
        let (sun, tz) = try grootkloofSun()
        // More than two hours left: nothing to say.
        XCTAssertNil(DaylightForecast.make(now: try local(2026, 6, 21, 14, 0, 0, tz), sun: sun, eta: nil, destination: nil, time: time))
        let late = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 15, 48, 0, tz), sun: sun, eta: nil,
                                                       destination: nil, time: time))
        XCTAssertEqual(late.text, "1 h 40 min of daylight left · sunset 17:27")
        XCTAssertEqual(late.level, .normal)
        let last = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 17, 18, 0, tz), sun: sun, eta: nil,
                                                       destination: nil, time: time))
        XCTAssertEqual(last.level, .amber)
        XCTAssertEqual(last.text, "10 min of daylight left · sunset 17:27")
        let dusk = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 17, 40, 0, tz), sun: sun, eta: nil,
                                                       destination: nil, time: time))
        XCTAssertEqual(dusk.text, "The sun has set · dark at 17:53")
        // After dark, without a route: gone.
        XCTAssertNil(DaylightForecast.make(now: try local(2026, 6, 21, 18, 30, 0, tz), sun: sun, eta: nil, destination: nil, time: time))
    }

    func testBannerWhileFollowing() throws {
        let (sun, tz) = try grootkloofSun()
        let now = try local(2026, 6, 21, 15, 17, 30, tz)
        let fine = try XCTUnwrap(DaylightForecast.make(now: now, sun: sun, eta: try local(2026, 6, 21, 16, 40, 0, tz),
                                                       destination: "Bannerman Hut", time: time))
        XCTAssertEqual(fine.text, "2 h 10 min of daylight left · at your pace you reach Bannerman Hut at 16:40")
        XCTAssertEqual(fine.level, .normal)
        XCTAssertEqual(try XCTUnwrap(fine.daylightLeft), 2 * 3600 + 10 * 60 + 25, accuracy: 60)
        // Within 15 min of sunset: amber.
        let tight = try XCTUnwrap(DaylightForecast.make(now: now, sun: sun, eta: try local(2026, 6, 21, 17, 20, 0, tz),
                                                        destination: "Bannerman Hut", time: time))
        XCTAssertEqual(tight.level, .amber)
        // After sunset, before dark: amber still.
        let dim = try XCTUnwrap(DaylightForecast.make(now: now, sun: sun, eta: try local(2026, 6, 21, 17, 45, 0, tz),
                                                      destination: "Bannerman Hut", time: time))
        XCTAssertEqual(dim.level, .amber)
        // After civil dusk: red, and how long after dark.
        let dark = try XCTUnwrap(DaylightForecast.make(now: now, sun: sun, eta: try local(2026, 6, 21, 18, 33, 0, tz),
                                                       destination: "the end of Bannerman Pass", time: time))
        XCTAssertEqual(dark.level, .red)
        XCTAssertEqual(dark.text, "2 h 10 min of daylight left · you reach the end of Bannerman Pass 40 min after dark")
        // Following in the dark: still said.
        let night = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 19, 0, 0, tz), sun: sun,
                                                        eta: try local(2026, 6, 21, 19, 30, 0, tz), destination: "Camp", time: time))
        XCTAssertTrue(night.text.hasPrefix("The sun has set · you reach Camp"), night.text)
        XCTAssertEqual(night.level, .red)
    }

    func testBannerOnPolarDays() throws {
        let tz = try zone("Europe/Oslo")
        let tromso = LatLon(lat: 69.6492, lon: 18.9553)
        let summer = SunTimes.compute(on: try local(2026, 6, 21, 12, 0, 0, tz), at: tromso, timeZone: tz)
        XCTAssertNil(DaylightForecast.make(now: try local(2026, 6, 21, 22, 0, 0, tz), sun: summer, eta: nil, destination: nil))
        let following = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 22, 0, 0, tz), sun: summer,
                                                             eta: try local(2026, 6, 21, 23, 30, 0, tz), destination: "Hut"))
        XCTAssertTrue(following.text.hasPrefix("The sun doesn't set today"), following.text)
        XCTAssertEqual(following.level, .normal, "no ETA warning in the midnight sun")
        XCTAssertNil(following.daylightLeft)
        let winter = SunTimes.compute(on: try local(2026, 12, 21, 12, 0, 0, tz), at: tromso, timeZone: tz)
        let dark = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 12, 21, 12, 0, 0, tz), sun: winter, eta: nil,
                                                       destination: nil))
        XCTAssertEqual(dark.text, "The sun doesn't rise today")
    }

    func testBannerBeforeDawn() throws {
        // An alpine start: the day ahead, not "14 h of daylight left" in the dark.
        let tz = try zone("Africa/Johannesburg")
        let start = try local(2026, 6, 21, 3, 0, 0, tz)
        let sun = SunTimes.around(start, at: LatLon(lat: -25.8405, lon: 27.3920), timeZone: tz)
        // This calculator's times for the day (PyEphem: within a minute).
        assertTime(sun.civilDawn, try local(2026, 6, 21, 6, 31, 19, tz), "dawn of the day ahead")
        assertTime(sun.sunrise, try local(2026, 6, 21, 6, 56, 33, tz), "sunrise of the day ahead")
        XCTAssertNil(sun.daylightLeft(at: start), "no daylight is left before it has begun")
        XCTAssertEqual(try XCTUnwrap(sun.timeToSunrise(at: start)), 3 * 3600 + 56 * 60 + 33, accuracy: 60)
        XCTAssertNil(sun.timeToSunrise(at: try local(2026, 6, 21, 12, 0, 0, tz)))

        let dark = try XCTUnwrap(DaylightForecast.make(now: start, sun: sun, eta: try local(2026, 6, 21, 6, 0, 0, tz),
                                                       destination: "Bannerman Hut", time: time))
        XCTAssertEqual(dark.text, "Dark until 06:31 · sunrise 06:56 · you reach Bannerman Hut at 06:00, before first light")
        XCTAssertEqual(dark.level, .red)
        XCTAssertNil(dark.daylightLeft)
        let twilight = try XCTUnwrap(DaylightForecast.make(now: start, sun: sun, eta: try local(2026, 6, 21, 6, 45, 0, tz),
                                                           destination: "Bannerman Hut", time: time))
        XCTAssertEqual(twilight.text, "Dark until 06:31 · sunrise 06:56 · at your pace you reach Bannerman Hut at 06:45")
        XCTAssertEqual(twilight.level, .amber)
        let day = try XCTUnwrap(DaylightForecast.make(now: start, sun: sun, eta: try local(2026, 6, 21, 9, 0, 0, tz),
                                                      destination: "Bannerman Hut", time: time))
        XCTAssertEqual(day.level, .normal)
        // A long day out: arriving after this evening's dusk is still red.
        let long = try XCTUnwrap(DaylightForecast.make(now: start, sun: sun, eta: try local(2026, 6, 21, 18, 33, 0, tz),
                                                       destination: "Camp", time: time))
        XCTAssertEqual(long.level, .red)
        XCTAssertTrue(long.text.hasSuffix("you reach Camp 40 min after dark"), long.text)

        // Without a route: nothing in the middle of the night, then the dark and the sunrise.
        XCTAssertNil(DaylightForecast.make(now: start, sun: sun, eta: nil, destination: nil, time: time))
        let early = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 5, 0, 0, tz), sun: sun, eta: nil,
                                                        destination: nil, time: time))
        XCTAssertEqual(early.text, "Dark until 06:31 · sunrise 06:56")
        XCTAssertEqual(early.level, .amber)
        let dawn = try XCTUnwrap(DaylightForecast.make(now: try local(2026, 6, 21, 6, 40, 0, tz), sun: sun, eta: nil,
                                                       destination: nil, time: time))
        XCTAssertEqual(dawn.text, "Sunrise at 06:56")
        XCTAssertEqual(dawn.level, .normal)
    }

    func testTheEveningGoesOnAfterMidnightAtHighLatitudes() throws {
        // Rovaniemi in early July: the sun sets at 00:54, after midnight, and the night never gets
        // darker than civil twilight. At 00:20 it is still the evening before; today's sunset is a
        // whole day away (this calculator's times).
        let tz = try zone("Europe/Helsinki")
        let rovaniemi = LatLon(lat: 66.5039, lon: 25.7294)
        let now = try local(2026, 7, 8, 0, 20, 0, tz)
        let sun = SunTimes.around(now, at: rovaniemi, timeZone: tz)
        assertTime(sun.sunset, try local(2026, 7, 8, 0, 53, 51, tz), "the evening's sunset")
        XCTAssertNil(sun.civilDusk)
        XCTAssertEqual(try XCTUnwrap(sun.daylightLeft(at: now)), 33 * 60 + 51, accuracy: 60)
        let following = try XCTUnwrap(DaylightForecast.make(now: now, sun: sun, eta: try local(2026, 7, 8, 1, 30, 0, tz),
                                                             destination: "Hut"))
        XCTAssertLessThan(try XCTUnwrap(following.daylightLeft), 3600, following.text)
        XCTAssertEqual(following.level, .amber, "you arrive after sunset")

        // After that sunset: today's sunrise (01:51) is next, and in the morning today's sunset.
        let night = SunTimes.around(try local(2026, 7, 8, 1, 10, 0, tz), at: rovaniemi, timeZone: tz)
        assertTime(night.sunrise, try local(2026, 7, 8, 1, 50, 45, tz), "today's sunrise")
        let morning = SunTimes.around(try local(2026, 7, 8, 9, 0, 0, tz), at: rovaniemi, timeZone: tz)
        assertTime(morning.sunset, try local(2026, 7, 9, 0, 45, 16, tz), "today's sunset, after midnight")
    }

    func testDurationWording() {
        XCTAssertEqual(RouteFormat.duration(2 * 3600 + 10 * 60), "2 h 10 min")
        XCTAssertEqual(RouteFormat.duration(2 * 3600 + 10), "2 h")
        XCTAssertEqual(RouteFormat.duration(45 * 60 + 20), "45 min")
        XCTAssertEqual(RouteFormat.duration(20), "under a minute")
        XCTAssertEqual(RouteFormat.duration(-5), "under a minute")
        XCTAssertEqual(RouteFormat.shortDuration(3 * 3600 + 5 * 60), "3:05")
        XCTAssertEqual(RouteFormat.duration(.infinity), "—")
    }

    func testPaceDescriptions() {
        XCTAssertEqual(PaceEstimate(factor: 1.12, source: .learned(tracks: 4)).description, "1.12× Tobler, learned from 4 tracks")
        XCTAssertEqual(PaceEstimate(factor: 1.3, source: .custom).description, "1.30× Tobler, set in Settings")
        XCTAssertTrue(PaceEstimate.model.description.contains("Tobler"))
        XCTAssertEqual(PaceEstimate(factor: 2, source: .custom).flatSpeed, PaceModel.toblerSpeed(slope: 0) / 2, accuracy: 1e-12)
    }
}
