import XCTest
@testable import MapMe

final class PlaceholderTests: XCTestCase {
    func testGeoRoundTrip() {
        let p = LatLon(lat: -25.85, lon: 27.40)
        let q = WebMercator.unproject(WebMercator.project(p))
        XCTAssertEqual(q.lat, p.lat, accuracy: 1e-9)
        XCTAssertEqual(q.lon, p.lon, accuracy: 1e-9)
    }
}
