import CoreGraphics
import Foundation
import XCTest
@testable import MapMe

/// Routes: geometry, walking times (Tobler, Naismith, the learned pace), following (progress on
/// loops and out-and-back routes, the off-route dwell and hysteresis), profiles from measured
/// elevations and from the terrain, and moving old map waypoints into the global store
/// (docs/design/explorer.md, Routes).
final class RoutesTests: XCTestCase {
    /// Grootkloof, the sample sheet.
    private let origin = LatLon(lat: -25.8405, lon: 27.3920)

    private func east(_ meters: Double, from p: LatLon? = nil) -> LatLon {
        Geodesy.destination(p ?? origin, bearing: 90, distance: meters)
    }

    private func north(_ meters: Double, from p: LatLon? = nil) -> LatLon {
        Geodesy.destination(p ?? origin, bearing: 0, distance: meters)
    }

    // MARK: - Walking time

    func testToblerAndNaismith() {
        // 6 · e^(−3.5 · 0.05) km/h on the flat: 5.04 km/h.
        XCTAssertEqual(PaceModel.toblerSpeed(slope: 0) * 3.6, 5.0364, accuracy: 1e-3)
        // Fastest 5 % downhill (6 km/h), symmetric about it.
        XCTAssertEqual(PaceModel.toblerSpeed(slope: -0.05) * 3.6, 6, accuracy: 1e-9)
        XCTAssertEqual(PaceModel.toblerSpeed(slope: 0.1), PaceModel.toblerSpeed(slope: -0.2), accuracy: 1e-12)
        // 10 % uphill: 6 · e^(−0.525) = 3.55 km/h.
        XCTAssertEqual(PaceModel.toblerSpeed(slope: 0.1) * 3.6, 3.5497, accuracy: 1e-3)
        XCTAssertEqual(PaceModel.toblerSpeed(slope: .nan), 0)
        // A kilometre on the flat: 714.8 s.
        XCTAssertEqual(PaceModel.toblerTime(meters: 1000, slope: 0), 714.8, accuracy: 0.1)
        // Naismith: 5 km/h plus an hour per 600 m up. 10 km and 600 m: 3 h.
        XCTAssertEqual(PaceModel.naismithTime(distanceM: 10_000, ascentM: 600), 3 * 3600, accuracy: 1e-6)
        XCTAssertEqual(PaceModel.naismithTime(distanceM: 5000, ascentM: 0), 3600, accuracy: 1e-6)
        // On a steady 10 % climb Naismith's rule is slower than Tobler's function.
        let tobler = PaceModel.toblerTime(meters: 6000, slope: 0.1)
        let naismith = PaceModel.naismithTime(distanceM: 6000, ascentM: 600)
        XCTAssertEqual(tobler, 6085, accuracy: 2)
        XCTAssertEqual(naismith, 7920, accuracy: 1e-6)
    }

    private func profile(_ points: [(Double, Double?)], distance: Double? = nil) -> ElevationProfile {
        let samples = points.map { ProfileSample(distanceM: $0.0, elevationM: $0.1) }
        return ElevationProfile(distanceM: distance ?? (points.last?.0 ?? 0), ascentM: nil, descentM: nil, minM: nil,
                                maxM: nil, samples: samples, vertexDistanceM: [], vertexElevationM: [], zoom: 13,
                                missingFraction: 0)
    }

    func testMovingTimeAlongAProfile() throws {
        let flat = profile([(0, 1500), (500, 1500), (1000, 1500)])
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: flat, factor: 1)), 714.8, accuracy: 0.1)
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: flat, factor: 1.2)), 857.7, accuracy: 0.2)
        // From halfway: half the time.
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: flat, factor: 1, fromDistanceM: 500)), 357.4, accuracy: 0.1)
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: flat, factor: 1, fromDistanceM: 750)), 178.7, accuracy: 0.1)
        // 1 km at 10 % up, then 1 km at 10 % down.
        let hill = profile([(0, 1000), (1000, 1100), (2000, 1000)])
        let up = PaceModel.toblerTime(meters: 1000, slope: 0.1)
        let down = PaceModel.toblerTime(meters: 1000, slope: -0.1)
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: hill, factor: 1)), up + down, accuracy: 1e-6)
        XCTAssertEqual(up, 1014.2, accuracy: 0.2)
        // 10 % down is as fast as the flat (Tobler is symmetric about 5 % down).
        XCTAssertEqual(down, 714.8, accuracy: 0.2)
        // A sample without elevation counts as flat; no elevations at all: nil.
        let gap = profile([(0, 1000), (1000, nil), (2000, 1000)])
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: gap, factor: 1)), 2 * 714.8, accuracy: 0.2)
        XCTAssertNil(PaceModel.movingTime(along: profile([(0, nil), (1000, nil)]), factor: 1))
        // A coarse profile stopping short of the line's length: the rest is flat.
        let short = profile([(0, 1500), (900, 1500)], distance: 1000)
        XCTAssertEqual(try XCTUnwrap(PaceModel.movingTime(along: short, factor: 1)), 714.8, accuracy: 0.2)
        XCTAssertEqual(PaceModel.flatTime(meters: 1000, factor: 2), 1429.6, accuracy: 0.2)
    }

    /// A walk east from `start` at `factor` × Tobler's flat speed, a fix every 10 s.
    private func walk(from start: LatLon, startTime: Date, meters: Double, factor: Double,
                      altitude: (Double) -> Double? = { _ in 1500 }) -> [TrackPoint] {
        let speed = PaceModel.toblerSpeed(slope: 0) / factor
        var points: [TrackPoint] = []
        var d = 0.0
        var t = 0.0
        while d <= meters {
            let p = east(d, from: start)
            points.append(TrackPoint(lat: p.lat, lon: p.lon, altitude: altitude(d), timestamp: startTime.addingTimeInterval(t),
                                     horizontalAccuracy: 5))
            d += speed * 10
            t += 10
        }
        return points
    }

    func testWalkPaceOfARecordedTrack() throws {
        let start = Date(timeIntervalSince1970: 1_780_000_000)
        let points = walk(from: origin, startTime: start, meters: 1500, factor: 1.25)
        let pace = PaceModel.walkPace(of: points)
        XCTAssertEqual(try XCTUnwrap(pace.factor), 1.25, accuracy: 0.005)
        XCTAssertGreaterThan(pace.movingMeters, 1400)

        // A 5-minute stop (gap over 2 min) and a car ride (8 m/s) are not walking.
        var stopped = points
        let last = stopped[stopped.count - 1]
        stopped.append(TrackPoint(lat: last.lat, lon: last.lon, altitude: 1500, timestamp: last.timestamp.addingTimeInterval(300),
                                  horizontalAccuracy: 5))
        let drive = east(800, from: last.latLon)
        stopped.append(TrackPoint(lat: drive.lat, lon: drive.lon, altitude: 1500,
                                  timestamp: last.timestamp.addingTimeInterval(400), horizontalAccuracy: 5))
        XCTAssertEqual(try XCTUnwrap(PaceModel.walkPace(of: stopped).factor), 1.25, accuracy: 0.005)
        XCTAssertEqual(PaceModel.walkPace(of: stopped).movingMeters, pace.movingMeters, accuracy: 1e-6)

        // Uphill at Tobler's own speed for the slope: factor 1.
        let slope = 0.1
        let v = PaceModel.toblerSpeed(slope: slope)
        var climb: [TrackPoint] = []
        for i in 0...150 {
            let d = Double(i) * v * 10
            let p = east(d)
            climb.append(TrackPoint(lat: p.lat, lon: p.lon, altitude: 1000 + d * slope,
                                    timestamp: start.addingTimeInterval(Double(i) * 10), horizontalAccuracy: 5))
        }
        XCTAssertEqual(try XCTUnwrap(PaceModel.walkPace(of: climb).factor), 1, accuracy: 0.01)
    }

    func testLearnedPaceIsTheMedianOfRecentTracks() throws {
        let day: TimeInterval = 86_400
        let base = Date(timeIntervalSince1970: 1_780_000_000)
        func track(_ factor: Double, daysAgo: Double, meters: Double = 1500) -> Track {
            let start = base.addingTimeInterval(-daysAgo * day)
            var t = Track(name: "Walk", startedAt: start)
            t.points = walk(from: origin, startTime: start, meters: meters, factor: factor)
            return t
        }
        XCTAssertNil(PaceModel.learnedFactor(from: []))
        // Under 1 km of moving says nothing.
        XCTAssertNil(PaceModel.learnedFactor(from: [track(1.3, daysAgo: 1, meters: 600)]))

        let tracks = [track(1.0, daysAgo: 3), track(1.5, daysAgo: 1), track(1.25, daysAgo: 2), track(1.1, daysAgo: 4, meters: 500)]
        let learned = try XCTUnwrap(PaceModel.learnedPace(from: tracks))
        XCTAssertEqual(learned.factor, 1.25, accuracy: 0.01)
        XCTAssertEqual(learned.source, .learned(tracks: 3))
        XCTAssertTrue(learned.description.contains("learned from 3 tracks"), learned.description)

        // Only the last ten count: an old quick walk is not among them…
        var many = (0..<10).map { track(1.8, daysAgo: Double($0)) }
        many.append(track(0.7, daysAgo: 30))
        XCTAssertEqual(try XCTUnwrap(PaceModel.learnedFactor(from: many)), 1.8, accuracy: 0.01)
        // …and the result stays within 0.6…2.
        XCTAssertEqual(try XCTUnwrap(PaceModel.learnedFactor(from: [track(2.6, daysAgo: 1)])), 2, accuracy: 1e-9)
    }

    func testLivePaceIsBlendedInUpToHalf() {
        let now = Date(timeIntervalSince1970: 1_780_000_000)
        // 45 min of walking at 1.5× ending now: half the weight.
        let speed = PaceModel.toblerSpeed(slope: 0) / 1.5
        let full = walk(from: origin, startTime: now.addingTimeInterval(-2700), meters: speed * 2700, factor: 1.5)
        XCTAssertEqual(PaceModel.blended(base: 1.0, recent: full, now: now), 1.25, accuracy: 0.01)
        // 22.5 min: a quarter.
        let half = walk(from: origin, startTime: now.addingTimeInterval(-1350), meters: speed * 1350, factor: 1.5)
        XCTAssertEqual(PaceModel.blended(base: 1.0, recent: half, now: now), 1.125, accuracy: 0.01)
        // Under 5 min of moving: the base alone.
        let short = walk(from: origin, startTime: now.addingTimeInterval(-200), meters: speed * 200, factor: 1.5)
        XCTAssertEqual(PaceModel.blended(base: 1.1, recent: short, now: now), 1.1, accuracy: 1e-9)
        // Walking long ago does not count.
        let old = walk(from: origin, startTime: now.addingTimeInterval(-7200), meters: speed * 1200, factor: 1.5)
        XCTAssertEqual(PaceModel.blended(base: 1.0, recent: old, now: now), 1.0, accuracy: 1e-9)
    }

    // MARK: - Geometry

    func testRouteLineDistancesAndNearestPoint() throws {
        let a = origin, b = east(1000), c = north(1000, from: b)
        let line = RouteLine([a, b, c])
        XCTAssertEqual(line.length, 2000, accuracy: 0.5)
        XCTAssertEqual(line.cumulative[1], 1000, accuracy: 0.5)
        let mid = try XCTUnwrap(line.point(atDistance: 1500))
        XCTAssertEqual(Geodesy.distance(mid, north(500, from: b)), 0, accuracy: 0.5)
        XCTAssertEqual(try XCTUnwrap(line.point(atDistance: -5)), a)
        XCTAssertEqual(try XCTUnwrap(line.point(atDistance: 99_999)), c)

        // 30 m south of the first segment, 400 m along.
        let p = Geodesy.destination(east(400), bearing: 180, distance: 30)
        let m = try XCTUnwrap(line.nearest(to: p))
        XCTAssertEqual(m.distanceAlongM, 400, accuracy: 0.5)
        XCTAssertEqual(m.distanceFromLineM, 30, accuracy: 0.5)
        XCTAssertEqual(m.segment, 0)
        // Restricted to the second segment.
        let far = try XCTUnwrap(line.nearest(to: p, alongRange: 1200...2000))
        XCTAssertEqual(far.segment, 1)
        XCTAssertEqual(far.distanceAlongM, 1000, accuracy: 1)
    }

    func testDouglasPeuckerKeepsCornersOnly() {
        // A right angle drawn with many points, a little wobble on each leg.
        var points: [CGPoint] = []
        for i in 0...50 { points.append(CGPoint(x: Double(i) * 2, y: i % 2 == 0 ? 0 : 0.3)) }
        for i in 1...50 { points.append(CGPoint(x: 100 + (i % 2 == 0 ? 0 : 0.3), y: Double(i) * 2)) }
        let keep = RouteGeometry.simplify(points, tolerance: 1)
        XCTAssertEqual(keep, [0, 50, 100])
        XCTAssertEqual(RouteGeometry.simplify(points, tolerance: 0.1).count, points.count)
        XCTAssertEqual(RouteGeometry.simplify([CGPoint(x: 1, y: 1)], tolerance: 1), [0])
    }

    func testClimbsCountWithHysteresis() {
        let ups = RouteGeometry.climbs([100, 101, 102, 101, 105, 103, 110, 100])
        // 100 → 105 (+5), → 110 (+5); then 110 → 100 (−10); the 1–2 m wiggles do not count.
        XCTAssertEqual(ups.ascent, 10, accuracy: 1e-9)
        XCTAssertEqual(ups.descent, 10, accuracy: 1e-9)
        XCTAssertEqual(RouteGeometry.climbs([]).ascent, 0)
    }

    // MARK: - Following

    func testOffRouteNeedsDwellAndTwoFixes() {
        var d = OffRouteDetector()
        let t0 = Date(timeIntervalSince1970: 1_780_000_000)
        XCTAssertEqual(OffRouteDetector.threshold(accuracyM: 5), 40)
        XCTAssertEqual(OffRouteDetector.threshold(accuracyM: 30), 60)
        XCTAssertEqual(OffRouteDetector.threshold(accuracyM: -1), 40)

        // 60 m off at ±5 m: not yet off before 20 s.
        for s in 0..<20 {
            XCTAssertEqual(d.update(distanceM: 60, accuracyM: 5, at: t0.addingTimeInterval(Double(s))), .unchanged, "at \(s) s")
        }
        XCTAssertFalse(d.isOffRoute)
        XCTAssertEqual(d.update(distanceM: 60, accuracyM: 5, at: t0.addingTimeInterval(20)), .left)
        XCTAssertTrue(d.isOffRoute)
        XCTAssertEqual(d.update(distanceM: 80, accuracyM: 5, at: t0.addingTimeInterval(21)), .unchanged)
        // Between 0.6 T and T (24…40 m): still off (hysteresis).
        XCTAssertEqual(d.update(distanceM: 30, accuracyM: 5, at: t0.addingTimeInterval(22)), .unchanged)
        XCTAssertTrue(d.isOffRoute)
        // Below 0.6 T: back on.
        XCTAssertEqual(d.update(distanceM: 20, accuracyM: 5, at: t0.addingTimeInterval(23)), .rejoined)
        XCTAssertFalse(d.isOffRoute)

        // A fix back within T breaks the run: the dwell starts again.
        var e = OffRouteDetector()
        _ = e.update(distanceM: 60, accuracyM: 5, at: t0)
        _ = e.update(distanceM: 30, accuracyM: 5, at: t0.addingTimeInterval(10))
        XCTAssertEqual(e.update(distanceM: 60, accuracyM: 5, at: t0.addingTimeInterval(25)), .unchanged)
        XCTAssertEqual(e.update(distanceM: 60, accuracyM: 5, at: t0.addingTimeInterval(44)), .unchanged)
        XCTAssertEqual(e.update(distanceM: 60, accuracyM: 5, at: t0.addingTimeInterval(45)), .left)

        // One fix 20 s after the last is not two fixes in a row: a single far fix never warns.
        var f = OffRouteDetector()
        XCTAssertEqual(f.update(distanceM: 500, accuracyM: 5, at: t0), .unchanged)
        _ = f.update(distanceM: 10, accuracyM: 5, at: t0.addingTimeInterval(1))
        XCTAssertEqual(f.update(distanceM: 500, accuracyM: 5, at: t0.addingTimeInterval(30)), .unchanged)

        // Poor accuracy widens the threshold: 50 m off at ±30 m is fine.
        var g = OffRouteDetector()
        for s in 0...30 { XCTAssertEqual(g.update(distanceM: 50, accuracyM: 30, at: t0.addingTimeInterval(Double(s))), .unchanged) }
    }

    func testProgressOnAnOutAndBackRoute() throws {
        // 1 km east and back the same way.
        let a = origin, b = east(1000)
        var tracker = RouteTracker(points: [a, b, a])
        let t0 = Date(timeIntervalSince1970: 1_780_000_000)
        func step(_ p: LatLon, _ s: Double) throws -> Double {
            let r = try XCTUnwrap(tracker.update(position: p, accuracyM: 5, at: t0.addingTimeInterval(s)))
            return r.match.distanceAlongM
        }
        XCTAssertEqual(try step(east(300), 0), 300, accuracy: 1)
        XCTAssertEqual(try step(east(600), 200), 600, accuracy: 1)
        XCTAssertEqual(try step(east(950), 450), 950, accuracy: 1)
        XCTAssertEqual(try step(east(1000), 500), 1000, accuracy: 1)
        // Walking back past the same places: the return leg.
        XCTAssertEqual(try step(east(950), 540), 1050, accuracy: 1)
        XCTAssertEqual(try step(east(800), 650), 1200, accuracy: 1)
        XCTAssertEqual(try step(east(100), 1200), 1900, accuracy: 1)
        XCTAssertFalse(tracker.isOffRoute)
    }

    func testProgressOnALoopAndAShortcut() throws {
        // A square loop of 4 × 500 m: start and end at the same place.
        let a = origin, b = east(500), c = north(500, from: b), d = north(500)
        var tracker = RouteTracker(points: [a, b, c, d, a])
        let t0 = Date(timeIntervalSince1970: 1_780_000_000)
        let first = try XCTUnwrap(tracker.update(position: east(10), accuracyM: 5, at: t0))
        XCTAssertEqual(first.match.distanceAlongM, 10, accuracy: 1)
        XCTAssertFalse(tracker.hasArrived(match: first.match, position: east(10)), "a loop starts where it ends")
        // A shortcut straight to the last leg (more than 500 m along ahead): found anyway.
        let late = north(300)
        let shortcut = try XCTUnwrap(tracker.update(position: late, accuracyM: 5, at: t0.addingTimeInterval(300)))
        XCTAssertEqual(shortcut.match.distanceAlongM, 1700, accuracy: 1)
        let home = try XCTUnwrap(tracker.update(position: north(15), accuracyM: 5, at: t0.addingTimeInterval(500)))
        XCTAssertEqual(home.match.distanceAlongM, 1985, accuracy: 1)
        XCTAssertTrue(tracker.hasArrived(match: home.match, position: north(15)))
    }

    func testOffRouteThroughTheTracker() throws {
        var tracker = RouteTracker(points: [origin, east(2000)])
        let t0 = Date(timeIntervalSince1970: 1_780_000_000)
        let off = Geodesy.destination(east(500), bearing: 0, distance: 120)
        var events: [OffRouteDetector.Event] = []
        for s in 0...25 {
            let r = try XCTUnwrap(tracker.update(position: off, accuracyM: 5, at: t0.addingTimeInterval(Double(s))))
            events.append(r.event)
            XCTAssertEqual(r.match.distanceFromLineM, 120, accuracy: 1)
        }
        XCTAssertEqual(events.filter { $0 == .left }.count, 1)
        XCTAssertEqual(events.firstIndex(of: .left), 20)
        XCTAssertTrue(tracker.isOffRoute)
    }

    func testNamedPointsAheadAndTheDestination() {
        let points = [
            RoutePoint(origin, isAnchor: true, name: "Car park"),
            RoutePoint(east(500), isAnchor: false),
            RoutePoint(east(1000), isAnchor: true, name: "Waterfall"),
            RoutePoint(east(2000), isAnchor: true, name: "Bannerman Hut"),
            RoutePoint(east(2500), isAnchor: true),
        ]
        let route = Route(name: "Bannerman Pass", kind: .planned, points: points)
        let line = RouteLine(route.latLons)
        XCTAssertEqual(RouteNavigator.nextNamedPoint(route: route, line: line, after: 100)?.name, "Waterfall")
        XCTAssertEqual(RouteNavigator.nextNamedPoint(route: route, line: line, after: 1200)?.name, "Bannerman Hut")
        XCTAssertEqual(RouteNavigator.nextNamedPoint(route: route, line: line, after: 2100)?.name, "the end of Bannerman Pass")
        // The banner names the last named point ahead: the hut, before the walk out.
        let dest = RouteNavigator.destination(route: route, line: line, after: 300)
        XCTAssertEqual(dest.name, "Bannerman Hut")
        XCTAssertEqual(dest.alongM, 2000, accuracy: 1)
        XCTAssertEqual(RouteNavigator.destination(route: route, line: line, after: 2200).name, "the end of Bannerman Pass")
        var named = route
        named.points[4].name = "Gate"
        XCTAssertEqual(RouteNavigator.destinationName(of: named), "Gate")
    }

    // MARK: - Profiles

    func testMeasuredProfileSmoothsAndCountsClimbs() throws {
        // 1 km rising steadily 100 m, with ±4 m of GPS-like noise on every other point.
        var points: [LatLon] = []
        var elevations: [Double?] = []
        for i in 0...100 {
            points.append(east(Double(i) * 10))
            elevations.append(Double(i) + (i % 2 == 0 ? 4 : -4))
        }
        let p = try XCTUnwrap(RoutePlanner.measuredProfile(points, elevations: elevations))
        XCTAssertEqual(p.distanceM, 1000, accuracy: 1)
        // The smoothing shortens the ramp at both ends a little (one-sided windows): 95 ± 3 m.
        XCTAssertEqual(try XCTUnwrap(p.ascentM), 95, accuracy: 3)
        XCTAssertEqual(try XCTUnwrap(p.descentM), 0, accuracy: 1e-9)
        XCTAssertEqual(try XCTUnwrap(p.minM), 2.4, accuracy: 2)
        XCTAssertEqual(try XCTUnwrap(p.maxM), 97.6, accuracy: 2)
        XCTAssertEqual(p.samples.first?.distanceM, 0)
        XCTAssertEqual(try XCTUnwrap(p.samples.last?.distanceM), 1000, accuracy: 1)
        XCTAssertNil(RoutePlanner.measuredProfile(points, elevations: points.map { _ in nil }))

        let climbs = try XCTUnwrap(RoutePlanner.remainingClimbs(of: p, fromDistanceM: 500))
        XCTAssertEqual(climbs.ascent, 47, accuracy: 4)
        XCTAssertEqual(try XCTUnwrap(RoutePlanner.elevation(in: p, atDistance: 500)), 50, accuracy: 1)
    }

    func testDistanceOnlyProfile() {
        let p = RoutePlanner.distanceOnlyProfile([origin, east(700), north(300, from: east(700))])
        XCTAssertEqual(p.distanceM, 1000, accuracy: 1)
        XCTAssertNil(p.ascentM)
        XCTAssertEqual(p.vertexDistanceM.count, 3)
        XCTAssertEqual(p.missingFraction, 1)
        XCTAssertNil(PaceModel.movingTime(along: p, factor: 1))
    }

    /// Fixture terrain (fixtures/grootkloof/dem) for the core's profile.
    private final class FixtureTiles: TerrainTileSource {
        func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)?) async -> [TileID: Data] {
            var out: [TileID: Data] = [:]
            for t in ts {
                if let png = Fixtures.demTile(t) { out[t] = png }
            }
            return out
        }
    }

    func testRouteStatsFromTheTerrain() async throws {
        let terrain = TerrainProvider(store: TerrainStore(), source: FixtureTiles())
        // About 2 km across the Grootkloof sheet, north-east.
        let points = [LatLon(lat: -25.8460, lon: 27.3870), LatLon(lat: -25.8405, lon: 27.3920),
                      LatLon(lat: -25.8290, lon: 27.4080)]
        let geodesic = RouteGeometry.length(points)
        let answered = await RoutePlanner.profile(of: points, terrain: terrain)
        let p = try XCTUnwrap(answered)
        XCTAssertEqual(p.distanceM, geodesic, accuracy: 1)
        XCTAssertEqual(p.vertexDistanceM.count, 3)
        if p.zoom == nil {
            // The engine answered nothing (not in this build): the distance alone, honestly.
            XCTAssertNil(p.ascentM)
            XCTAssertNil(PaceModel.movingTime(along: p, factor: 1))
        } else {
            let ascent = try XCTUnwrap(p.ascentM), descent = try XCTUnwrap(p.descentM)
            XCTAssertGreaterThan(ascent + descent, 5, "the Magaliesberg is not flat")
            XCTAssertLessThan(ascent + descent, 1500)
            XCTAssertEqual(try XCTUnwrap(p.minM), 1450, accuracy: 450)
            let route = Route(name: "Test", kind: .planned, points: points.map { RoutePoint($0, isAnchor: true) })
            let stats = RoutePlanner.stats(for: route, profile: p)
            XCTAssertEqual(stats.geometryHash, route.geometryHash)
            XCTAssertEqual(stats.ascentM, ascent)
            XCTAssertNotNil(PaceModel.movingTime(along: p, factor: 1))
        }
        // The best profile of a GPX route without terrain falls back to the file's elevations.
        let empty = TerrainProvider(store: TerrainStore(), source: EmptyTiles())
        var gpx = Route(name: "From a file", kind: .gpxRoute, points: [])
        gpx.points = (0...40).map { i in RoutePoint(lat: -25.84, lon: 27.39 + Double(i) * 0.0005, ele: 1400 + Double(i) * 2,
                                                    isAnchor: true) }
        let fromFile = await RoutePlanner.bestProfile(for: gpx, terrain: empty)
        let best = try XCTUnwrap(fromFile)
        if best.source == .file {
            // 80 m up over 2 km; the smoothing trims a metre or two at each end.
            XCTAssertEqual(try XCTUnwrap(best.profile.ascentM), 78, accuracy: 4)
        } else {
            XCTAssertEqual(best.source, .terrain, "terrain or the file's elevations, never nothing")
        }
    }

    private final class EmptyTiles: TerrainTileSource {
        func tiles(_ ts: [TileID], progress: ((Int, Int) -> Void)?) async -> [TileID: Data] { [:] }
    }

    // MARK: - Migration and stores

    private func temporaryDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("routes-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @MainActor
    func testOldMapWaypointsMoveIntoTheStoreOnce() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root.appendingPathComponent("Maps", isDirectory: true))
        let store = GeoItemStore(directory: root.appendingPathComponent("GeoItems", isDirectory: true))
        let mine = MapPOI(name: "Lunch spot", kind: .waypoint, source: .user, position: origin, elevationM: 1480)
        let label = MapPOI(name: "Grootkloof", kind: .place, source: .mapLabel, position: east(300), elevationM: nil)
        let doc = MapDocument(id: UUID(), name: "Grootkloof", createdAt: Date(timeIntervalSince1970: 1_700_000_000),
                              lastOpenedAt: nil, sourceFileName: "source.pdf", sourceKind: .pdf, pixelWidth: 100,
                              pixelHeight: 100, renderDPI: 300, tileSize: 512, levels: 1, georef: nil, georefMessage: nil,
                              pois: [mine, label], contourRGB: nil)
        library.save(doc)

        GeoItemMigration.migrateMapWaypoints(library: library, store: store)
        XCTAssertEqual(store.waypoints.count, 1)
        let moved = try XCTUnwrap(store.waypoint(id: mine.id))
        XCTAssertEqual(moved.name, "Lunch spot")
        XCTAssertEqual(moved.elevationM, 1480)
        XCTAssertEqual(moved.origin, .migrated(mapID: doc.id))
        XCTAssertEqual(library.map(id: doc.id)?.pois.map(\.name), ["Grootkloof"])

        // Again: nothing changes.
        GeoItemMigration.migrateMapWaypoints(library: library, store: store)
        XCTAssertEqual(store.waypoints.count, 1)
        store.flush()
        XCTAssertEqual(GeoItemStore(directory: store.directory).waypoints.map(\.id), [mine.id])
        XCTAssertEqual(MapLibrary(rootURL: library.rootURL).map(id: doc.id)?.pois.count, 1)
    }

    func testTheMapToShowImportedItemsOn() throws {
        let truth = try GrootkloofTruth.load()
        let size = 1000
        func doc(_ name: String, scale: Double) -> MapDocument {
            var d = MapDocument(id: UUID(), name: name, createdAt: Date(), lastOpenedAt: nil, sourceFileName: "source.pdf",
                                sourceKind: .pdf, pixelWidth: Int(Double(size) * scale), pixelHeight: Int(Double(size) * scale),
                                renderDPI: 300, tileSize: 512, levels: 1, georef: nil, georefMessage: nil, pois: [],
                                contourRGB: nil)
            var g = truth.georeference
            // The same place at `scale` × the pixels: finer.
            let m = g.pixelToMercator.m
            g.pixelToMercator = Homography([m[0] / scale, m[1] / scale, m[2], m[3] / scale, m[4] / scale, m[5],
                                            m[6] / scale, m[7] / scale, m[8]])
            g.warp = nil
            d.georef = g
            return d
        }
        let coarse = doc("Overview", scale: 1)
        let fine = doc("Detail", scale: 2)
        let inside = try XCTUnwrap(coarse.center)
        XCTAssertEqual(GPXMapChooser.bestMap(for: [inside], maps: [coarse, fine]), fine.id)
        XCTAssertNil(GPXMapChooser.bestMap(for: [LatLon(lat: 10, lon: 10)], maps: [coarse, fine]))
    }
}
