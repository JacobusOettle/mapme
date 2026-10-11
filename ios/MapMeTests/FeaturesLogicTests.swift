import CoreGraphics
import Foundation
import XCTest
@testable import MapMe

/// Non-UI logic behind Calibration, Nearby and Settings.
final class FeaturesLogicTests: XCTestCase {
    // MARK: - Calibration fit

    private func graticulePair(_ truth: GrootkloofTruth, lat: Int, lon: Int, gps: Bool = false) -> CalibrationPair {
        let la = truth.latLines[lat]
        let lo = truth.lonLines[lon]
        return CalibrationPair(pixel: CGPoint(x: lo.x, y: la.y), coordinate: LatLon(lat: la.lat, lon: lo.lon), fromGPS: gps)
    }

    func testCalibrationFitFromThreeGraticulePoints() throws {
        let truth = try GrootkloofTruth.load()
        let pairs = [
            graticulePair(truth, lat: 0, lon: 0),
            graticulePair(truth, lat: 2, lon: 2),
            graticulePair(truth, lat: 4, lon: 1, gps: true),
        ]
        let fit = try CalibrationFit.make(from: pairs).get()
        XCTAssertEqual(fit.georef.method, .manual)
        XCTAssertEqual(fit.residuals.count, 3)
        XCTAssertLessThan(fit.rms, 5, "rms \(fit.rms) m")
        XCTAssertEqual(fit.georef.confidence, CalibrationFit.confidence(forRMS: fit.rms))

        // An intersection that was not used for the fit.
        let check = graticulePair(truth, lat: 3, lon: 0)
        let predicted = fit.georef.latLon(forPixel: check.pixel)
        let error = Geodesy.distance(predicted, check.coordinate)
        XCTAssertLessThan(error, 10, "check point off by \(error) m")
    }

    func testCalibrationFitNeedsTwoDistinctPoints() throws {
        let truth = try GrootkloofTruth.load()
        let a = graticulePair(truth, lat: 0, lon: 0)
        let b = graticulePair(truth, lat: 4, lon: 2)

        switch CalibrationFit.make(from: [a]) {
        case .success: XCTFail("one point must not fit")
        case .failure: break
        }

        let two = try CalibrationFit.make(from: [a, b]).get()
        XCTAssertEqual(two.residuals.count, 2)
        let mid = graticulePair(truth, lat: 2, lon: 1)
        XCTAssertLessThan(Geodesy.distance(two.georef.latLon(forPixel: mid.pixel), mid.coordinate), 15)

        switch CalibrationFit.make(from: [a, a]) {
        case .success(let fit):
            XCTFail("coincident points must not fit (rms \(fit.rms))")
        case .failure(let error):
            XCTAssertFalse(error.message.isEmpty)
        }
    }

    func testConfidenceAndResidualThresholds() {
        XCTAssertEqual(CalibrationFit.confidence(forRMS: 3), 0.9)
        XCTAssertEqual(CalibrationFit.confidence(forRMS: 12), 0.7)
        XCTAssertEqual(CalibrationFit.confidence(forRMS: 45), 0.5)
    }

    // MARK: - Contour colour sampling

    /// The window read must use top-left image coordinates, like everything else.
    func testColourWindowOrientation() throws {
        let width = 20, height = 10
        let ctx = try XCTUnwrap(TileStore.makeContext(width: width, height: height))
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        // Image pixel (x 3, y 2) from the top-left; CG's origin is bottom-left.
        ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        ctx.fill(CGRect(x: 3, y: height - 1 - 2, width: 1, height: 1))
        let image = try XCTUnwrap(ctx.makeImage())

        let px = try XCTUnwrap(ContourColorSampler.rgba(of: image, x: 2, y: 1, width: 3, height: 3))
        XCTAssertEqual(px.count, 3 * 3 * 4)
        for row in 0..<3 {
            for col in 0..<3 {
                let i = (row * 3 + col) * 4
                if row == 1 && col == 1 {
                    XCTAssertGreaterThan(px[i], 200)
                    XCTAssertLessThan(px[i + 1], 60)
                } else {
                    XCTAssertGreaterThan(px[i + 1], 200, "white expected at \(col),\(row)")
                }
            }
        }
    }

    /// Taps a few pixels off real contour lines of the Grootkloof sheet must return the
    /// sheet's contour colour (truth.json: 168, 152, 136), not paper, text or water.
    func testContourColourPickOnGrootkloof() throws {
        let image = try XCTUnwrap(TileStore.loadImage(at: Fixtures.url("map.jpg")))
        let reference = [168.0, 152.0, 136.0]
        let taps = [(1350, 2077), (1777, 2614), (1096, 3141), (2324, 1591), (2531, 2079)]
        var good = 0
        for (x, y) in taps {
            let px = try XCTUnwrap(ContourColorSampler.rgba(of: image, x: x - 6, y: y - 6, width: 13, height: 13))
            let rgb = ContourColorSampler.pick(px, width: 13, height: 13, centerX: 6, centerY: 6)
            XCTAssertEqual(rgb.count, 3)
            let d = zip(rgb, reference).map { (Double($0) - $1) * (Double($0) - $1) }.reduce(0, +).squareRoot()
            if d < 35 { good += 1 }
        }
        XCTAssertGreaterThanOrEqual(good, 4, "contour colour picked for \(good)/\(taps.count) taps")
    }

    // MARK: - Nearby

    func testNearbyRowsSortNearestFirst() {
        let here = LatLon(lat: -25.85, lon: 27.40)
        let far = MapPOI(name: "Far", kind: .peak, source: .mapLabel,
                         position: Geodesy.destination(here, bearing: 90, distance: 3000), elevationM: 1500)
        let near = MapPOI(name: "Near", kind: .water, source: .openStreetMap,
                          position: Geodesy.destination(here, bearing: 0, distance: 200), elevationM: nil)
        let mid = MapPOI(name: "Mid", kind: .waypoint, source: .user,
                         position: Geodesy.destination(here, bearing: 225, distance: 1000), elevationM: nil)

        let rows = NearbyRow.rows(for: [far, near, mid], from: here)
        XCTAssertEqual(rows.map(\.poi.name), ["Near", "Mid", "Far"])
        XCTAssertEqual(rows[0].distance ?? 0, 200, accuracy: 2)
        let b0 = rows[0].bearing ?? -1
        XCTAssertLessThan(min(abs(b0), abs(360 - b0)), 1, "bearing \(b0)")
        XCTAssertEqual(rows[1].bearing ?? -1, 225, accuracy: 1)

        let unsorted = NearbyRow.rows(for: [far, near, mid], from: nil)
        XCTAssertEqual(unsorted.map(\.poi.name), ["Far", "Near", "Mid"])
        XCTAssertNil(unsorted[0].distance)
    }

    func testNearbyFilters() {
        let p = LatLon(lat: 0, lon: 0)
        let peak = MapPOI(name: "A", kind: .peak, source: .mapLabel, position: p, elevationM: nil)
        let spot = MapPOI(name: "B", kind: .spotHeight, source: .mapLabel, position: p, elevationM: nil)
        let water = MapPOI(name: "C", kind: .water, source: .openStreetMap, position: p, elevationM: nil)
        let place = MapPOI(name: "D", kind: .place, source: .mapLabel, position: p, elevationM: nil)
        let mine = MapPOI(name: "E", kind: .waypoint, source: .user, position: p, elevationM: nil)
        let all = [peak, spot, water, place, mine]

        func names(_ f: NearbyFilter) -> [String] { all.filter { f.includes($0) }.map(\.name) }
        XCTAssertEqual(names(.all), ["A", "B", "C", "D", "E"])
        XCTAssertEqual(names(.peaks), ["A", "B"])
        XCTAssertEqual(names(.water), ["C"])
        XCTAssertEqual(names(.places), ["D"])
        XCTAssertEqual(names(.mine), ["E"])
    }

    // MARK: - Settings

    func testDirectorySize() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("size-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("a/b", isDirectory: true),
                                                withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try Data(count: 10_000).write(to: dir.appendingPathComponent("one.bin"))
        try Data(count: 20_000).write(to: dir.appendingPathComponent("a/b/two.bin"))

        XCTAssertGreaterThanOrEqual(StorageUsage.directorySize(dir), 30_000)
        XCTAssertEqual(StorageUsage.directorySize(dir.appendingPathComponent("missing")), 0)
    }

    @MainActor
    func testStatusChipText() {
        XCTAssertEqual(StatusChip.text(for: nil), "Not located")
        let g = Georeference(pixelToMercator: .identity, method: .contours, confidence: 0.8, rmsMeters: 18.4)
        XCTAssertEqual(StatusChip.text(for: g), "Contours \u{00B1}18 m")
        let manual = Georeference(pixelToMercator: .identity, method: .manual, confidence: 0.9, rmsMeters: nil)
        XCTAssertEqual(StatusChip.text(for: manual), "Manual")
    }
}
