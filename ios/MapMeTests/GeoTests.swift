import CoreGraphics
import XCTest
@testable import MapMe

final class GeoTests: XCTestCase {
    func testWebMercatorRoundTrip() {
        for p in [LatLon(lat: -25.85, lon: 27.40), LatLon(lat: 0, lon: 0), LatLon(lat: 60.5, lon: -179.9), LatLon(lat: -84, lon: 179.9)] {
            let q = WebMercator.unproject(WebMercator.project(p))
            XCTAssertEqual(q.lat, p.lat, accuracy: 1e-9)
            XCTAssertEqual(q.lon, p.lon, accuracy: 1e-9)
        }
        let edge = WebMercator.project(LatLon(lat: 0, lon: 180))
        XCTAssertEqual(edge.x, 20_037_508.342789, accuracy: 1e-3)
        XCTAssertEqual(edge.y, 0, accuracy: 1e-6)
    }

    func testHomographyInverse() throws {
        let h = Homography([1.2, 0.3, 100, -0.2, 0.9, -50, 1e-5, 2e-5, 1])
        let inv = try XCTUnwrap(h.inverse)
        for p in [SIMD2<Double>(0, 0), SIMD2<Double>(1000, 250), SIMD2<Double>(-300, 4000), SIMD2<Double>(3507, 4960)] {
            let q = inv.apply(h.apply(p))
            XCTAssertEqual(q.x, p.x, accuracy: 1e-6)
            XCTAssertEqual(q.y, p.y, accuracy: 1e-6)
        }
        XCTAssertNil(Homography([1, 2, 3, 2, 4, 6, 0, 0, 0]).inverse)
    }

    func testNorthAngleOnRotatedSimilarity() {
        let origin = WebMercator.project(LatLon(lat: -25.85, lon: 27.40))
        for degrees in [0.0, 30.0, -45.0, 120.0] {
            let a = degrees * .pi / 180
            let s = 2.1
            // North on the image points `a` clockwise from image-up.
            let h = Homography([s * cos(a), s * sin(a), origin.x, s * sin(a), -s * cos(a), origin.y, 0, 0, 1])
            let g = Georeference(pixelToMercator: h, method: .manual, confidence: 1, rmsMeters: nil)
            XCTAssertEqual(g.northAngle(atPixel: CGPoint(x: 500, y: 700)), a, accuracy: 1e-4, "rotation \(degrees)°")
            // Round trip pixel ↔ coordinate.
            let ll = g.latLon(forPixel: CGPoint(x: 1234, y: 567))
            let px = g.pixel(for: ll)
            XCTAssertEqual(Double(px.x), 1234, accuracy: 1e-5)
            XCTAssertEqual(Double(px.y), 567, accuracy: 1e-5)
        }
    }

    func testMetersPerPixelOnTruth() throws {
        let truth = try GrootkloofTruth.load()
        let g = truth.georeference
        // 1:25 000 at 300 dpi in Web Mercator: 2.118 Mercator m/px × cos(25.85°).
        XCTAssertEqual(g.metersPerPixel(atPixel: CGPoint(x: 1754, y: 2488)), 1.906, accuracy: 0.01)
        XCTAssertEqual(g.northAngle(atPixel: CGPoint(x: 1754, y: 2488)), 0, accuracy: 1e-3)
        // Graticule intersection 25°49'S 27°23'E is at pixel (878, 541).
        let ll = g.latLon(forPixel: CGPoint(x: 878, y: 541))
        XCTAssertEqual(ll.lat, -(25 + 49.0 / 60), accuracy: 2e-5)
        XCTAssertEqual(ll.lon, 27 + 23.0 / 60, accuracy: 2e-5)
    }

    func testUTM() {
        let u = UTM.from(LatLon(lat: -25.85, lon: 27.40))
        XCTAssertEqual(u.zone, 35)
        XCTAssertEqual(u.band, "J")
        XCTAssertEqual(u.easting, 540_082, accuracy: 1)
        XCTAssertEqual(u.northing, 7_140_867, accuracy: 1)
    }

    func testGeodesy() {
        let a = LatLon(lat: 0, lon: 0)
        XCTAssertEqual(Geodesy.distance(a, LatLon(lat: 0, lon: 1)), 111_195, accuracy: 1)
        XCTAssertEqual(Geodesy.bearing(a, LatLon(lat: 1, lon: 0)), 0, accuracy: 1e-9)
        XCTAssertEqual(Geodesy.bearing(a, LatLon(lat: 0, lon: 1)), 90, accuracy: 1e-9)
        let start = Fixtures.center
        let b = Geodesy.destination(start, bearing: 45, distance: 12_000)
        XCTAssertEqual(Geodesy.distance(start, b), 12_000, accuracy: 0.5)
        XCTAssertEqual(Geodesy.bearing(start, b), 45, accuracy: 0.1)
        XCTAssertEqual(Geodesy.compassPoint(45), "NE")
        XCTAssertEqual(Geodesy.compassPoint(359), "N")
        XCTAssertEqual(Geodesy.compassPoint(200), "SSW")
    }

    func testGeoFormat() {
        XCTAssertEqual(GeoFormat.distance(850, units: .metric), "850 m")
        XCTAssertEqual(GeoFormat.distance(3210, units: .metric), "3.21 km")
        XCTAssertEqual(GeoFormat.distance(12_345, units: .metric), "12.3 km")
        XCTAssertEqual(GeoFormat.distance(100, units: .imperial), "328 ft")
        XCTAssertEqual(GeoFormat.distance(1609.344 * 2, units: .imperial), "2.00 mi")
        XCTAssertEqual(GeoFormat.elevation(1523.4, units: .metric), "1523 m")
        XCTAssertEqual(GeoFormat.elevation(1000, units: .imperial), "3281 ft")
        XCTAssertEqual(GeoFormat.speed(1, units: .metric), "3.6 km/h")
        XCTAssertEqual(GeoFormat.duration(65), "1:05")
        XCTAssertEqual(GeoFormat.duration(3725), "1:02:05")
        XCTAssertEqual(GeoFormat.coordinate(LatLon(lat: -25.85, lon: 27.4), format: .decimal), "-25.85000, 27.40000")
        XCTAssertEqual(GeoFormat.coordinate(LatLon(lat: -25.5, lon: 27.25), format: .dms), "25°30′00.0″S  27°15′00.0″E")
        XCTAssertEqual(GeoFormat.coordinate(LatLon(lat: -25.85, lon: 27.4), format: .utm), "35J 540082E 7140867N")
    }
}
