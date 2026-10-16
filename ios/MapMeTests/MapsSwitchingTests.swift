import CoreGraphics
import CoreLocation
import Foundation
import UIKit
import XCTest
@testable import MapMe

/// The maps feature's rules (docs/design/explorer.md §7.3) on synthetic georeferences: coverage,
/// the best map, auto-switching with its dwell clocks, cool-down and exclusions, the continuation
/// margin, where you are relative to a sheet, scale texts and other maps' places.
final class MapsSwitchingTests: XCTestCase {
    /// Grootkloof's centre.
    static let centre = LatLon(lat: -25.85, lon: 27.40)

    /// A north-up map `width` × `height` pixels at `metersPerPixel` ground metres per pixel,
    /// centred on `centre`; `perspective` tilts it like a photo (w = 1 + a·u, u from the centre).
    static func map(name: String = "Map", width: Int, height: Int, metersPerPixel: Double,
                    centre: LatLon = MapsSwitchingTests.centre, perspective: Double = 0,
                    located: Bool = true) -> MapDocument {
        let c = WebMercator.project(centre)
        let s = metersPerPixel / WebMercator.groundPerMercator(lat: centre.lat)
        let w = Double(width)
        let h = Double(height)
        // pixel → centred (u, v, 1) → perspective (u, v, 1 + a·u) → Mercator (scale, flip y, move).
        let toCentre = [1, 0, -w / 2, 0, 1, -h / 2, 0, 0, 1]
        let tilt = [1, 0, 0, 0, 1, 0, perspective, 0, 1]
        let place = [s, 0, c.x, 0, -s, c.y, 0, 0, 1]
        let m = multiply(place, multiply(tilt, toCentre))
        let georef = Georeference(pixelToMercator: Homography(m), method: .grid, confidence: 0.9, rmsMeters: 3,
                                  createdAt: Date(timeIntervalSince1970: 1_700_000_000))
        return MapDocument(id: UUID(), name: name, createdAt: Date(timeIntervalSince1970: 1_700_000_000), lastOpenedAt: nil,
                           sourceFileName: "source.pdf", sourceKind: .pdf, pixelWidth: width, pixelHeight: height,
                           renderDPI: nil, tileSize: 512, levels: 3, georef: located ? georef : nil, georefMessage: nil,
                           pois: [], contourRGB: nil)
    }

    static func multiply(_ a: [Double], _ b: [Double]) -> [Double] {
        var out = [Double](repeating: 0, count: 9)
        for r in 0..<3 {
            for c in 0..<3 {
                out[r * 3 + c] = (0..<3).reduce(0) { $0 + a[r * 3 + $1] * b[$1 * 3 + c] }
            }
        }
        return out
    }

    /// `metres` north of `p`.
    static func north(of p: LatLon, _ metres: Double) -> LatLon {
        Geodesy.destination(p, bearing: 0, distance: metres)
    }

    // MARK: - Coverage

    func testCoverageInsideAndOutside() throws {
        // 2000 px at 10 m/px: 20 km square around the centre.
        let a = Self.map(width: 2000, height: 2000, metersPerPixel: 10)
        let mid = try XCTUnwrap(MapSwitcher.coverage(of: a, at: Self.centre))
        XCTAssertEqual(mid.metersPerPixel, 10, accuracy: 0.1)
        XCTAssertEqual(mid.insideMarginM, 10_000, accuracy: 100)
        XCTAssertEqual(mid.mapID, a.id)

        let nearTop = try XCTUnwrap(MapSwitcher.coverage(of: a, at: Self.north(of: Self.centre, 9_800)))
        XCTAssertEqual(nearTop.insideMarginM, 200, accuracy: 15)

        let beyond = try XCTUnwrap(MapSwitcher.coverage(of: a, at: Self.north(of: Self.centre, 11_000)))
        XCTAssertEqual(beyond.insideMarginM, -1_000, accuracy: 20, "negative outside: the distance to the edge")
        XCTAssertEqual(beyond.metersPerPixel, 10, accuracy: 0.2)

        XCTAssertNil(MapSwitcher.coverage(of: Self.map(width: 100, height: 100, metersPerPixel: 1, located: false),
                                          at: Self.centre))
    }

    func testCoverageIgnoresTheFarSideOfAPhotosHorizon() throws {
        // w = 1 + 1e-3·u crosses zero 1000 px left of the centre: places mapping beyond that line
        // are behind the camera, not on the sheet's plane.
        let photo = Self.map(width: 1000, height: 800, metersPerPixel: 2, perspective: 1e-3)
        let g = try XCTUnwrap(photo.georef)
        XCTAssertNotNil(MapSwitcher.coverage(of: photo, at: Self.centre))
        let behind = WebMercator.unproject(g.mercator(forPixel: SIMD2(-2_500, 400)))
        XCTAssertTrue(behind.lat.isFinite && behind.lon.isFinite)
        XCTAssertNil(MapSwitcher.coverage(of: photo, at: behind))
    }

    func testCoveringListsTheMostDetailedFirst() {
        let overview = Self.map(name: "Overview", width: 2000, height: 2000, metersPerPixel: 10)
        let detail = Self.map(name: "Detail", width: 1000, height: 1000, metersPerPixel: 2.5)
        let elsewhere = Self.map(name: "Elsewhere", width: 1000, height: 1000, metersPerPixel: 1,
                                 centre: LatLon(lat: -29.1, lon: 29.4))
        let here = MapSwitcher.covering(Self.centre, maps: [overview, elsewhere, detail])
        XCTAssertEqual(here.map(\.mapID), [detail.id, overview.id])
        // 2 km north is beyond the detail sheet (1.25 km from its centre to its edges).
        let north = MapSwitcher.covering(Self.north(of: Self.centre, 2_000), maps: [overview, detail])
        XCTAssertEqual(north.map(\.mapID), [overview.id])
    }

    // MARK: - Best map

    func testBestMapIsTheFinestThenTheCurrentWithinTenPercentThenTheMostRecent() {
        let a = UUID(), b = UUID(), c = UUID()
        let list = [MapCoverage(mapID: a, metersPerPixel: 10, insideMarginM: 500),
                    MapCoverage(mapID: b, metersPerPixel: 2.5, insideMarginM: 500),
                    MapCoverage(mapID: c, metersPerPixel: 2.7, insideMarginM: 500)]
        let old = Date(timeIntervalSince1970: 1_000)
        let new = Date(timeIntervalSince1970: 2_000)
        XCTAssertEqual(AutoSwitchPolicy.best(list, currentID: a, recency: [:])?.mapID, b)
        XCTAssertEqual(AutoSwitchPolicy.best(list, currentID: c, recency: [:])?.mapID, c, "2.7 is within 10 % of 2.5")
        XCTAssertEqual(AutoSwitchPolicy.best(list, currentID: a, recency: [b: old, c: new])?.mapID, c,
                       "equally detailed: the most recently opened")
        XCTAssertEqual(AutoSwitchPolicy.best(list, currentID: a, recency: [b: new, c: old])?.mapID, b)
        XCTAssertNil(AutoSwitchPolicy.best([], currentID: a, recency: [:]))
    }

    // MARK: - Auto-switching

    private let current = UUID()
    private let finer = UUID()
    private let t0 = Date(timeIntervalSince1970: 1_800_000_000)

    private func fix(_ at: Date, accuracy: Double = 5) -> AutoSwitchPolicy.Fix {
        AutoSwitchPolicy.Fix(accuracyM: accuracy, timestamp: at)
    }

    /// You are well inside both: the current map at 10 m/px, the other at 2.5 m/px.
    private var bothCover: [MapCoverage] {
        [MapCoverage(mapID: current, metersPerPixel: 10, insideMarginM: 3_000),
         MapCoverage(mapID: finer, metersPerPixel: 2.5, insideMarginM: 400)]
    }

    /// A policy showing `current`, opened long enough ago for the grace to be over.
    private func settledPolicy() -> AutoSwitchPolicy {
        var p = AutoSwitchPolicy()
        p.opened(current, at: t0.addingTimeInterval(-600))
        return p
    }

    private func step(_ p: inout AutoSwitchPolicy, _ seconds: Double, _ coverages: [MapCoverage],
                      enabled: Bool = true, following: Bool = true, idle: Bool = true, accuracy: Double = 5,
                      fixAge: Double = 0) -> MapSwitchDecision? {
        let now = t0.addingTimeInterval(seconds)
        return p.evaluate(currentID: p.currentID ?? current, coverages: coverages,
                          fix: fix(now.addingTimeInterval(-fixAge), accuracy: accuracy), enabled: enabled,
                          following: following, idle: idle, recency: [:], now: now)
    }

    func testSwitchesToAMoreDetailedMapAfterTwentySeconds() {
        var p = settledPolicy()
        XCTAssertNil(step(&p, 0, bothCover))
        XCTAssertNil(step(&p, 10, bothCover))
        XCTAssertNil(step(&p, 19, bothCover))
        XCTAssertEqual(step(&p, 20, bothCover), MapSwitchDecision(mapID: finer, reason: .moreDetailed))
    }

    func testLeavingTheFinerMapRestartsItsClock() {
        var p = settledPolicy()
        XCTAssertNil(step(&p, 0, bothCover))
        // 15 s later you are only 60 m inside: below the 100 m margin, the clock stops.
        let edge = [bothCover[0], MapCoverage(mapID: finer, metersPerPixel: 2.5, insideMarginM: 60)]
        XCTAssertNil(step(&p, 15, edge))
        XCTAssertNil(step(&p, 16, bothCover))
        XCTAssertNil(step(&p, 35, bothCover))
        XCTAssertEqual(step(&p, 36, bothCover)?.mapID, finer)
    }

    func testOnlyWhileFollowingIdleEnabledAndWithAGoodFix() {
        for variant in 0..<5 {
            var p = settledPolicy()
            var decision: MapSwitchDecision?
            for t in stride(from: 0.0, through: 40, by: 1) {
                switch variant {
                case 0: decision = step(&p, t, bothCover, following: false)
                case 1: decision = step(&p, t, bothCover, idle: false)
                case 2: decision = step(&p, t, bothCover, enabled: false)
                case 3: decision = step(&p, t, bothCover, accuracy: 60)
                default: decision = step(&p, t, bothCover, fixAge: 12)
                }
                XCTAssertNil(decision, "variant \(variant) switched at \(t) s")
            }
        }
        // The dwell clock runs while a tool is open: closing it switches at once.
        var p = settledPolicy()
        for t in stride(from: 0.0, through: 25, by: 1) {
            XCTAssertNil(step(&p, t, bothCover, idle: false))
        }
        XCTAssertEqual(step(&p, 26, bothCover)?.mapID, finer)
    }

    func testNeedsToBeOneAndAHalfTimesFiner() {
        var p = settledPolicy()
        let slightly = [bothCover[0], MapCoverage(mapID: finer, metersPerPixel: 7, insideMarginM: 400)]
        for t in stride(from: 0.0, through: 60, by: 5) {
            XCTAssertNil(step(&p, t, slightly), "10 / 7 is less than 1.5×")
        }
        var q = settledPolicy()
        let enough = [bothCover[0], MapCoverage(mapID: finer, metersPerPixel: 6.6, insideMarginM: 400)]
        XCTAssertNil(step(&q, 0, enough))
        XCTAssertEqual(step(&q, 20, enough)?.mapID, finer)
    }

    func testAMapOpenedFromTheLibraryStaysForThirtySeconds() {
        var p = AutoSwitchPolicy()
        p.opened(current, at: t0)
        XCTAssertNil(step(&p, 0, bothCover))
        XCTAssertNil(step(&p, 20, bothCover), "dwell done, but the grace is not")
        XCTAssertNil(step(&p, 29, bothCover))
        XCTAssertEqual(step(&p, 30, bothCover)?.mapID, finer)
    }

    func testAtMostOneSwitchAMinute() {
        var p = settledPolicy()
        XCTAssertNil(step(&p, 0, bothCover))
        XCTAssertEqual(step(&p, 20, bothCover)?.mapID, finer)
        p.switched(to: finer, at: t0.addingTimeInterval(20))
        // An even finer map covers you: still a minute after the last switch.
        let finest = UUID()
        let three = bothCover + [MapCoverage(mapID: finest, metersPerPixel: 1, insideMarginM: 300)]
        XCTAssertNil(step(&p, 21, three))
        XCTAssertNil(step(&p, 79, three))
        XCTAssertEqual(step(&p, 80, three)?.mapID, finest)
    }

    func testWalkingOffSwitchesAfterFiveSecondsEvenToACoarserMap() {
        var p = settledPolicy()
        let other = UUID()
        // 40 m beyond the current sheet's edge; the other (coarser) map has you 45 m inside.
        let off = [MapCoverage(mapID: current, metersPerPixel: 2.5, insideMarginM: -40),
                   MapCoverage(mapID: other, metersPerPixel: 10, insideMarginM: 45)]
        XCTAssertNil(step(&p, 0, off))
        XCTAssertNil(step(&p, 4, off))
        XCTAssertEqual(step(&p, 5, off), MapSwitchDecision(mapID: other, reason: .walkedOff))
    }

    func testWalkingOffNeedsAnotherMapAndTheMargin() {
        var p = settledPolicy()
        let alone = [MapCoverage(mapID: current, metersPerPixel: 2.5, insideMarginM: -400)]
        for t in stride(from: 0.0, through: 30, by: 5) {
            XCTAssertNil(step(&p, t, alone), "nothing to switch to: the terrain continues the map")
        }
        // Only 20 m off (less than 30 m): not off yet.
        var q = settledPolicy()
        let edge = [MapCoverage(mapID: current, metersPerPixel: 2.5, insideMarginM: -20),
                    MapCoverage(mapID: UUID(), metersPerPixel: 10, insideMarginM: 500)]
        for t in stride(from: 0.0, through: 30, by: 5) {
            XCTAssertNil(step(&q, t, edge))
        }
        // With 45 m accuracy, 40 m off is not enough either.
        var r = settledPolicy()
        let fuzzy = [MapCoverage(mapID: current, metersPerPixel: 2.5, insideMarginM: -40),
                     MapCoverage(mapID: UUID(), metersPerPixel: 10, insideMarginM: 500)]
        for t in stride(from: 0.0, through: 30, by: 5) {
            XCTAssertNil(step(&r, t, fuzzy, accuracy: 45))
        }
    }

    func testUndoLeavesTheMapAloneUntilYouWalkOffIt() {
        var p = settledPolicy()
        XCTAssertNil(step(&p, 0, bothCover))
        XCTAssertEqual(step(&p, 20, bothCover)?.mapID, finer)
        p.switched(to: finer, at: t0.addingTimeInterval(20))
        p.undid(finer, backTo: current, at: t0.addingTimeInterval(22))
        XCTAssertEqual(p.currentID, current)
        for t in stride(from: 23.0, through: 300, by: 10) {
            XCTAssertNil(step(&p, t, bothCover), "excluded after Undo (\(t) s)")
        }
        // You walk off the finer sheet: the exclusion ends; back on it, the usual 20 s apply.
        let leftIt = [bothCover[0], MapCoverage(mapID: finer, metersPerPixel: 2.5, insideMarginM: -300)]
        XCTAssertNil(step(&p, 310, leftIt))
        XCTAssertTrue(p.excluded.isEmpty)
        XCTAssertNil(step(&p, 320, bothCover))
        XCTAssertNil(step(&p, 339, bothCover))
        XCTAssertEqual(step(&p, 340, bothCover)?.mapID, finer)
    }

    func testUndoExclusionLastsFifteenMinutesAtMost() {
        var p = settledPolicy()
        p.undid(finer, backTo: current, at: t0)
        XCTAssertNil(step(&p, 1, bothCover))
        XCTAssertNil(step(&p, 899, bothCover))
        // It has covered you all along: once the exclusion is over, the switch comes at once.
        XCTAssertEqual(step(&p, 900, bothCover)?.mapID, finer)
    }

    func testAMapYouPickedStaysUntilYouLeaveIt() {
        var p = AutoSwitchPolicy()
        p.chose(current, at: t0.addingTimeInterval(-600))
        for t in stride(from: 0.0, through: 120, by: 10) {
            XCTAssertNil(step(&p, t, bothCover), "picked by hand")
        }
        // Walking off the picked map ends the hold: the walked-off rule switches.
        let off = [MapCoverage(mapID: current, metersPerPixel: 10, insideMarginM: -100),
                   MapCoverage(mapID: finer, metersPerPixel: 2.5, insideMarginM: 400)]
        XCTAssertNil(step(&p, 130, off))
        XCTAssertNil(p.heldUntil)
        XCTAssertEqual(step(&p, 135, off), MapSwitchDecision(mapID: finer, reason: .walkedOff))
    }

    func testComingBackToTheScreenKeepsAMapYouPicked() {
        var p = AutoSwitchPolicy()
        p.chose(current, at: t0.addingTimeInterval(-600))   // held until t0 + 300 s
        XCTAssertNil(step(&p, 0, bothCover))
        // Back from the world view: the same map's screen appears again.
        p.opened(current, at: t0.addingTimeInterval(60))
        XCTAssertNotNil(p.heldUntil, "the pick is kept")
        for t in stride(from: 60.0, through: 290, by: 10) {
            XCTAssertNil(step(&p, t, bothCover), "picked by hand (\(t) s)")
        }
        // The 15 min are over; the finer map has covered you all along.
        XCTAssertEqual(step(&p, 300, bothCover)?.mapID, finer)
        // Opening another map is a fresh start.
        p.opened(finer, at: t0.addingTimeInterval(310))
        XCTAssertEqual(p.currentID, finer)
        XCTAssertNil(p.heldUntil)
    }

    func testNeverSwitchesAwayFromAMapThatIsNotLocated() {
        // A trail brochure that could not be located, opened while you stand on a located sheet.
        var p = settledPolicy()
        let other = [MapCoverage(mapID: finer, metersPerPixel: 2.5, insideMarginM: 400)]
        for t in stride(from: 0.0, through: 120, by: 5) {
            let now = t0.addingTimeInterval(t)
            XCTAssertNil(p.evaluate(currentID: current, currentIsLocated: false, coverages: other, fix: fix(now),
                                    enabled: true, following: true, idle: true, recency: [:], now: now), "\(t) s")
        }
        // A located map that does not cover you is another matter: you walked off it.
        var q = settledPolicy()
        XCTAssertNil(step(&q, 0, other))
        XCTAssertEqual(step(&q, 5, other), MapSwitchDecision(mapID: finer, reason: .walkedOff))
    }

    func testAnotherScreenStartsOver() {
        var p = settledPolicy()
        XCTAssertNil(step(&p, 0, bothCover))
        // The screen now shows `finer` without telling the policy (another screen opened it).
        let now = t0.addingTimeInterval(25)
        let coarse = UUID()
        let decision = p.evaluate(currentID: finer, coverages: bothCover + [MapCoverage(mapID: coarse, metersPerPixel: 1,
                                                                                         insideMarginM: 500)],
                                  fix: fix(now), enabled: true, following: true, idle: true, recency: [:], now: now)
        XCTAssertNil(decision, "a fresh opening has its 30 s grace")
        XCTAssertEqual(p.currentID, finer)
    }

    // MARK: - Continuation

    func testContinuationMarginInMetres() {
        XCTAssertEqual(MapContinuation.marginMeters(longSideM: 2_000), 3_000)
        XCTAssertEqual(MapContinuation.marginMeters(longSideM: 10_000), 5_000)
        XCTAssertEqual(MapContinuation.marginMeters(longSideM: 150_000), 15_000)
        XCTAssertEqual(MapContinuation.marginMeters(longSideM: .infinity), 3_000)
    }

    func testContinuationMarginInPixels() throws {
        // 20 km × 16 km at 10 m/px: half the long side, 10 km = 1000 px.
        let a = Self.map(width: 2000, height: 1600, metersPerPixel: 10)
        XCTAssertEqual(try XCTUnwrap(MapContinuation.longSideMeters(of: a)), 20_000, accuracy: 100)
        XCTAssertEqual(Double(MapContinuation.marginPixels(for: a, enabled: true)), 1_000, accuracy: 10)
        XCTAssertEqual(MapContinuation.marginPixels(for: a, enabled: false), 0)
        XCTAssertEqual(MapContinuation.marginPixels(for: Self.map(width: 100, height: 100, metersPerPixel: 1, located: false),
                                                    enabled: true), 0)
        // A 2 km sheet at 1 m/px still gets 3 km (3000 px).
        let small = Self.map(width: 2000, height: 1000, metersPerPixel: 1)
        XCTAssertEqual(Double(MapContinuation.marginPixels(for: small, enabled: true)), 3_000, accuracy: 30)
        let drawable = MapContinuation.drawableRect(for: small, enabled: true)
        XCTAssertEqual(Double(drawable.minX), -3_000, accuracy: 30)
        XCTAssertEqual(Double(drawable.width), 8_000, accuracy: 60)
    }

    func testContinuationStaysWhereAPhotosPerspectiveHolds() throws {
        // w = 1 + 2e-4·u over u = ±1000: the 25 % bound allows 1250 − 1000 = 250 px beyond the edge.
        let photo = Self.map(width: 2000, height: 2000, metersPerPixel: 10, perspective: 2e-4)
        let margin = Double(MapContinuation.marginPixels(for: photo, enabled: true))
        XCTAssertEqual(margin, 250, accuracy: 2)
        let m = try XCTUnwrap(photo.georef).pixelToMercator.m
        let wc = m[6] * 1000 + m[7] * 1000 + m[8]
        for (x, y) in [(-margin, -margin), (2000 + margin, -margin), (2000 + margin, 2000 + margin), (-margin, 2000 + margin)] {
            let w = m[6] * x + m[7] * y + m[8]
            XCTAssertLessThanOrEqual(abs(w - wc), 0.25 * abs(wc) + 1e-9)
        }
    }

    // MARK: - Whereabouts

    func testWhereYouAreRelativeToTheSheet() throws {
        let a = Self.map(width: 2000, height: 2000, metersPerPixel: 10)
        let margin = MapContinuation.marginPixels(for: a, enabled: true)
        func at(_ p: LatLon, accuracy: Double = 5) -> MapWhereabouts? {
            MapWhereabouts.of(a, fix: CLLocation(coordinate: p.coordinate, altitude: 0, horizontalAccuracy: accuracy,
                                                  verticalAccuracy: 5, timestamp: Date()), continuation: margin)
        }
        XCTAssertEqual(at(Self.centre), .onSheet)
        // The edge is ≈ 9 993 m north (Web Mercator against the mean Earth radius).
        XCTAssertEqual(at(Self.north(of: Self.centre, 10_000)), .onSheet, "7 m beyond the edge is GPS noise")
        guard case .offSheet(let off)? = at(Self.north(of: Self.centre, 10_800)) else {
            return XCTFail("800 m beyond the edge is off the sheet, in the terrain")
        }
        XCTAssertEqual(off, 800, accuracy: 20)
        guard case .beyond(let far)? = at(Self.north(of: Self.centre, 25_000)) else {
            return XCTFail("15 km beyond the edge is beyond the 10 km continuation")
        }
        XCTAssertEqual(far, 15_000, accuracy: 200)
        XCTAssertNil(MapWhereabouts.of(a, fix: nil, continuation: margin))
        // No continuation: off the sheet is beyond it.
        guard case .beyond? = MapWhereabouts.of(a, fix: CLLocation(latitude: Self.north(of: Self.centre, 10_800).lat,
                                                                    longitude: Self.centre.lon), continuation: 0)
        else { return XCTFail("without terrain beyond the sheet, off it is beyond") }
    }

    // MARK: - Texts and helpers

    func testScaleTexts() {
        // 4.2333 m/px at 300 dpi is 1:50 000.
        XCTAssertEqual(MapScaleText.text(metersPerPixel: 4.2333, dpi: 300), "1:50\u{2009}000")
        XCTAssertEqual(MapScaleText.text(metersPerPixel: 2.1, dpi: 300), "1:25\u{2009}000")
        XCTAssertEqual(MapScaleText.text(metersPerPixel: 4, dpi: nil), "4 m/px")
        XCTAssertEqual(MapScaleText.text(metersPerPixel: 2.54, dpi: nil), "2.5 m/px")
        XCTAssertEqual(MapScaleText.text(metersPerPixel: 25.4, dpi: nil), "25 m/px")
        XCTAssertEqual(MapScaleText.roundedScale(49_870), 50_000)
        XCTAssertEqual(MapScaleText.roundedScale(24_100), 24_000)
        XCTAssertEqual(MapScaleText.groupedThousands(250_000), "250\u{2009}000")
        XCTAssertEqual(MapScaleText.groupedThousands(500), "500")
    }

    func testOtherMapsPlacesAreDeduplicatedByNameWithin150m() {
        var index = OtherMapsPOIOverlay.DuplicateIndex()
        let peak = MapPOI(name: "Tonquani", kind: .peak, source: .mapLabel, position: Self.centre)
        index.insert(peak)
        let near = MapPOI(name: "tonquani ", kind: .place, source: .openStreetMap,
                          position: Self.north(of: Self.centre, 120))
        let far = MapPOI(name: "Tonquani", kind: .peak, source: .mapLabel, position: Self.north(of: Self.centre, 400))
        let other = MapPOI(name: "Castle Gorge", kind: .place, source: .mapLabel, position: Self.centre)
        XCTAssertTrue(index.contains(near), "the same name within 150 m, case and spaces aside")
        XCTAssertFalse(index.contains(far))
        XCTAssertFalse(index.contains(other))
        let spring = MapPOI(name: "", kind: .water, source: .openStreetMap, position: Self.centre)
        index.insert(spring)
        XCTAssertTrue(index.contains(MapPOI(name: "", kind: .water, source: .openStreetMap,
                                            position: Self.north(of: Self.centre, 30))))
        XCTAssertFalse(index.contains(MapPOI(name: "", kind: .parking, source: .openStreetMap, position: Self.centre)))
    }

    func testTerrainDetailFollowsTheScale() {
        let lat = Self.centre.lat
        XCTAssertEqual(TerrainContourRenderer.demZoom(metersPerPoint: 2.5, latitude: lat, cap: 13), 12)
        XCTAssertEqual(TerrainContourRenderer.demZoom(metersPerPoint: 1, latitude: lat, cap: 13), 13,
                       "zoomed in past 2 m/pt: zoom 13")
        XCTAssertEqual(TerrainContourRenderer.demZoom(metersPerPoint: 1, latitude: lat, cap: 12), 12, "battery saver")
        XCTAssertEqual(TerrainContourRenderer.demZoom(metersPerPoint: 100, latitude: lat, cap: 13), 11)
        XCTAssertEqual(TerrainContourRenderer.demZoom(metersPerPoint: 5_000, latitude: lat, cap: 13), 8)
        XCTAssertEqual(TerrainContourRenderer.rgba(UIColor(red: 1, green: 0, blue: 0.5, alpha: 1)), [255, 0, 128, 255])
    }

    #if DEBUG
    @MainActor
    func testOverviewCloneKeepsTheGroundUnderEachPixel() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("maps-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        var sample = Self.map(name: MapsDebugSeeds.sampleName, width: 4000, height: 3000, metersPerPixel: 2)
        sample.levels = 4
        let tiles = library.tilesDirectory(for: sample, dark: false)
        for level in 0..<4 {
            let dir = tiles.appendingPathComponent("\(level)", isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try Data([UInt8(level)]).write(to: dir.appendingPathComponent("0_0.jpg"))
        }
        library.save(sample)

        let made = await MapsDebugSeeds.clone(sample, library: library)
        let overview = try XCTUnwrap(made)
        XCTAssertEqual(overview.name, MapsDebugSeeds.overviewName)
        XCTAssertEqual(overview.pixelWidth, 1000)
        XCTAssertEqual(overview.pixelHeight, 750)
        XCTAssertEqual(overview.levels, 2)
        let level0 = library.tilesDirectory(for: overview, dark: false).appendingPathComponent("0/0_0.jpg")
        XCTAssertEqual(try Data(contentsOf: level0), Data([2]), "the overview's level 0 is the sample's level 2")
        let g = try XCTUnwrap(sample.georef)
        let h = try XCTUnwrap(overview.georef)
        for p in [CGPoint(x: 0, y: 0), CGPoint(x: 500, y: 300), CGPoint(x: 1000, y: 750)] {
            let a = g.latLon(forPixel: CGPoint(x: p.x * 4, y: p.y * 4))
            let b = h.latLon(forPixel: p)
            XCTAssertLessThan(Geodesy.distance(a, b), 0.01)
        }
        XCTAssertNotNil(library.map(id: overview.id))
    }
    #endif
}
