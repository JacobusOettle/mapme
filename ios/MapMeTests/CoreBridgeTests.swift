import CoreGraphics
import XCTest
@testable import MapMe

final class CoreBridgeTests: XCTestCase {
    func testVersion() {
        let v = MapMeCore.version
        XCTAssertFalse(v.isEmpty)
        XCTAssertNotEqual(v, "?")
    }

    func testFitControlPointsFromTruth() throws {
        let truth = try GrootkloofTruth.load()
        let top = try XCTUnwrap(truth.latLines.first)
        let bottom = try XCTUnwrap(truth.latLines.last)
        let left = try XCTUnwrap(truth.lonLines.first)
        let right = try XCTUnwrap(truth.lonLines.last)
        let points = [
            ControlPointInput(x: left.x, y: top.y, lat: top.lat, lon: left.lon),
            ControlPointInput(x: right.x, y: top.y, lat: top.lat, lon: right.lon),
            ControlPointInput(x: left.x, y: bottom.y, lat: bottom.lat, lon: left.lon),
        ]
        let response = try MapMeCore.fitControlPoints(ControlPointRequest(points: points, model: "auto"))
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        let m = try XCTUnwrap(response.pixelToMercator)
        XCTAssertEqual(m.count, 9)
        let h = Homography(m)
        // Every graticule intersection, plus the frame corners and centre.
        var worst = 0.0
        for la in truth.latLines {
            for lo in truth.lonLines {
                let fitted = WebMercator.unproject(h.apply(SIMD2(lo.x, la.y)))
                worst = max(worst, Geodesy.distance(fitted, LatLon(lat: la.lat, lon: lo.lon)))
            }
        }
        worst = max(worst, truth.maxGroundError(of: h, scale: 1))
        XCTAssertLessThan(worst, 2.0, "max error \(worst) m")
    }

    func testFitControlPointsRejectsInvalidInput() throws {
        let response = try MapMeCore.fitControlPoints(ControlPointRequest(points: [], model: "auto"))
        XCTAssertEqual(response.status, "failed")
    }

    func testBlankPageFailsCleanly() throws {
        // A plain white page: no grid, no contours. Must fail cleanly, never crash.
        guard let ctx = TileStore.makeContext(width: 400, height: 300) else { return XCTFail("context") }
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 400, height: 300))
        let image = try XCTUnwrap(ctx.makeImage())
        let session = try GeoreferenceService.makeSession(from: image)
        var request = GeorefRequest()
        request.methods = ["grid"]
        let response = try Fixtures.georeference(session, request)
        XCTAssertEqual(response.status, "failed")
        XCTAssertNil(response.georeference)
    }
}
