import CoreLocation
import Foundation
import XCTest
@testable import MapMe

/// The battery saver (docs/design/explorer.md, Battery saver): when it is on, what it asks of
/// CoreLocation (and that CoreLocation gets it), what the rest of the app may do, and the measured
/// drain.
final class BatteryTests: XCTestCase {
    func testWhenTheSaverIsOn() {
        XCTAssertFalse(BatterySaver.isActive(mode: .off, lowPowerMode: true))
        XCTAssertFalse(BatterySaver.isActive(mode: .auto, lowPowerMode: false))
        XCTAssertTrue(BatterySaver.isActive(mode: .auto, lowPowerMode: true))
        XCTAssertTrue(BatterySaver.isActive(mode: .on, lowPowerMode: false))
        XCTAssertTrue(BatterySaver.mocksLowPowerMode(["-mockLowPowerMode"]))
        XCTAssertFalse(BatterySaver.mocksLowPowerMode(["-mockLocation", "1,2"]))
    }

    func testLocationProfiles() {
        let normal = LocationPowerProfile.normal
        XCTAssertEqual(normal.desiredAccuracy, kCLLocationAccuracyBest)
        XCTAssertEqual(normal.distanceFilter, kCLDistanceFilterNone)
        XCTAssertEqual(normal.headingFilter, 1)
        XCTAssertTrue(normal.headingUpdates)
        // Off: as MapMe always ran, in front or behind.
        for background in [false, true] {
            XCTAssertEqual(BatterySaver.profile(saverActive: false, inBackground: background, mapOnScreen: true, freshFix: false),
                           normal)
        }

        // On, a map on screen: still the GPS (±10 m), a fix per 5 m, 5° compass steps.
        let onMap = BatterySaver.profile(saverActive: true, inBackground: false, mapOnScreen: true, freshFix: false)
        XCTAssertEqual(onMap.desiredAccuracy, kCLLocationAccuracyNearestTenMeters)
        XCTAssertEqual(onMap.distanceFilter, 5)
        XCTAssertEqual(onMap.headingFilter, 5)
        XCTAssertTrue(onMap.headingUpdates)
        // The heartbeat lets one fix through whatever the distance.
        let fresh = BatterySaver.profile(saverActive: true, inBackground: false, mapOnScreen: true, freshFix: true)
        XCTAssertEqual(fresh.distanceFilter, kCLDistanceFilterNone)
        XCTAssertEqual(fresh.desiredAccuracy, kCLLocationAccuracyNearestTenMeters)
        // No map on screen (the library, Settings): no compass.
        XCTAssertFalse(BatterySaver.profile(saverActive: true, inBackground: false, mapOnScreen: false, freshFix: false).headingUpdates)
        // In the background (recording, following): 10 m, no compass, whatever the map.
        let behind = BatterySaver.profile(saverActive: true, inBackground: true, mapOnScreen: true, freshFix: true)
        XCTAssertEqual(behind.desiredAccuracy, kCLLocationAccuracyNearestTenMeters)
        XCTAssertEqual(behind.distanceFilter, 10)
        XCTAssertFalse(behind.headingUpdates)
    }

    @MainActor
    func testProfilesReachCoreLocation() {
        let saver = BatterySaver.profile(saverActive: true, inBackground: false, mapOnScreen: true, freshFix: false)
        let manager = CLLocationManager()
        LocationService.configure(manager, with: saver)
        XCTAssertEqual(manager.desiredAccuracy, kCLLocationAccuracyNearestTenMeters)
        XCTAssertEqual(manager.distanceFilter, 5)
        XCTAssertEqual(manager.headingFilter, 5)
        LocationService.configure(manager, with: .normal)
        XCTAssertEqual(manager.desiredAccuracy, kCLLocationAccuracyBest)
        XCTAssertEqual(manager.distanceFilter, kCLDistanceFilterNone)
        XCTAssertEqual(manager.headingFilter, 1)

        // The service keeps what it was given (idle while hosting unit tests: no manager).
        let service = LocationService()
        XCTAssertEqual(service.powerProfile, .normal)
        service.apply(saver)
        XCTAssertEqual(service.powerProfile, saver)
    }

    @MainActor
    func testWhatTheAppMayDo() {
        XCTAssertEqual(BatteryLimits.normal.maxFrameRate, 120)
        XCTAssertTrue(BatteryLimits.normal.animationsEnabled)
        XCTAssertTrue(BatteryLimits.normal.allowsKeepScreenAwake)
        XCTAssertEqual(BatteryLimits.normal.maxTerrainZoom, 13)
        XCTAssertTrue(BatteryLimits.normal.allowsPrefetch)
        XCTAssertEqual(BatteryLimits.saver.maxFrameRate, 30)
        XCTAssertFalse(BatteryLimits.saver.animationsEnabled)
        XCTAssertFalse(BatteryLimits.saver.allowsKeepScreenAwake)
        XCTAssertEqual(BatteryLimits.saver.maxTerrainZoom, 12)
        XCTAssertFalse(BatteryLimits.saver.allowsPrefetch)
        XCTAssertFalse(BatteryLimits.saver.allowsMultisampling)

        // Not attached to the app: off, and the canvas runs as before.
        let saver = BatterySaver()
        XCTAssertFalse(saver.isActive)
        XCTAssertEqual(saver.limits, .normal)
        XCTAssertEqual(saver.maxFrameRate, 120)
        XCTAssertNil(saver.drainText)
    }

    func testMeasuredDrain() throws {
        var meter = BatteryDrainMeter()
        let start = Date(timeIntervalSince1970: 1_800_000_000)
        func at(_ minutes: Double) -> Date { start.addingTimeInterval(minutes * 60) }

        meter.record(level: 0.80, unplugged: true, at: at(0))
        meter.record(level: 0.80, unplugged: true, at: at(5))
        XCTAssertNil(meter.rate, "nothing dropped yet")
        meter.record(level: 0.79, unplugged: true, at: at(10))
        XCTAssertNil(meter.rate, "under 15 minutes")
        meter.record(level: 0.78, unplugged: true, at: at(20))
        // From the first sample: 2 % in 20 min.
        var rate = try XCTUnwrap(meter.rate)
        XCTAssertEqual(rate.perHour, 0.06, accuracy: 1e-9)
        XCTAssertEqual(rate.minutes, 20)
        meter.record(level: 0.77, unplugged: true, at: at(30))
        meter.record(level: 0.77, unplugged: true, at: at(38))
        // Between the level changes (10 → 30 min): exact, whatever the reporting steps.
        rate = try XCTUnwrap(meter.rate)
        XCTAssertEqual(rate.perHour, 0.06, accuracy: 1e-9)
        XCTAssertEqual(rate.minutes, 20)

        // Unknown levels (the Simulator) are ignored.
        meter.record(level: -1, unplugged: true, at: at(40))
        XCTAssertNotNil(meter.rate)

        // Plugged in: starts over.
        meter.record(level: 0.77, unplugged: false, at: at(45))
        XCTAssertNil(meter.rate)
        XCTAssertNil(meter.first)

        // Charged while MapMe wasn't looking: starts over from the higher level.
        meter.record(level: 0.50, unplugged: true, at: at(60))
        meter.record(level: 0.60, unplugged: true, at: at(120))
        XCTAssertEqual(meter.first?.level, 0.60)
        XCTAssertNil(meter.rate)
    }
}
