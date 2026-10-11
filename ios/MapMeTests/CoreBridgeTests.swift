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

    func testPageDetectionJSON() throws {
        let ok = #"{"status":"ok","quad":[[10,20],[110,22],[108,180],[12,178]],"confidence":0.83,"coverage":0.4}"#
        let page = try XCTUnwrap(MapMeCore.decodePageDetection(ok, width: 200, height: 200))
        let expected: [CGPoint] = [CGPoint(x: 10.0, y: 20.0), CGPoint(x: 110.0, y: 22.0),
                                   CGPoint(x: 108.0, y: 180.0), CGPoint(x: 12.0, y: 178.0)]
        XCTAssertEqual(page.quad, expected)
        XCTAssertEqual(page.confidence, 0.83, accuracy: 1e-12)
        XCTAssertEqual(page.coverage, 0.4, accuracy: 1e-12)
        // Without a coverage it is worked out from the quad.
        let bare = #"{"status":"ok","quad":[[0,0],[100,0],[100,50],[0,50]],"confidence":0.7}"#
        let computed = try XCTUnwrap(MapMeCore.decodePageDetection(bare, width: 200, height: 100))
        XCTAssertEqual(computed.coverage, 0.25, accuracy: 1e-12)
        XCTAssertNil(MapMeCore.decodePageDetection(#"{"status":"failed","message":"no page"}"#, width: 10, height: 10))
        let threeCorners = #"{"status":"ok","quad":[[0,0],[1,0],[1,1]],"confidence":0.9,"coverage":0.5}"#
        XCTAssertNil(MapMeCore.decodePageDetection(threeCorners, width: 10, height: 10))
        XCTAssertNil(MapMeCore.decodePageDetection("not json", width: 10, height: 10))
    }

    func testDetectPageRejectsBadInputAndBlankImages() throws {
        let bytes = [UInt8](repeating: 255, count: 16)
        bytes.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) -> Void in
            // Too few bytes for 4 × 4, and an empty raster.
            XCTAssertNil(MapMeCore.detectPage(rgba: buffer, width: 4, height: 4, bytesPerRow: 16))
            XCTAssertNil(MapMeCore.detectPage(rgba: buffer, width: 0, height: 1, bytesPerRow: 0))
        }
        // A plain white page: nothing stands out, and the call returns cleanly.
        let white = [UInt8](repeating: 255, count: 320 * 240 * 4)
        let found = white.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) -> PageDetection? in
            MapMeCore.detectPage(rgba: buffer, width: 320, height: 240, bytesPerRow: 320 * 4)
        }
        XCTAssertNil(found)
    }
}
