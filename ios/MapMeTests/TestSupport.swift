import CoreGraphics
import Foundation
import XCTest
@testable import MapMe

/// Anchor class for locating the test bundle.
final class FixtureBundleToken {}

enum FixtureError: Error, CustomStringConvertible {
    case missing(String)
    var description: String {
        switch self {
        case .missing(let s): return "Fixture missing: \(s)"
        }
    }
}

/// fixtures/grootkloof, copied into the test bundle as the folder "grootkloof".
enum Fixtures {
    /// Grootkloof map centre (from the core tests).
    static let center = LatLon(lat: -25.8499, lon: 27.4000)

    static func directory() throws -> URL {
        guard let url = Bundle(for: FixtureBundleToken.self).url(forResource: "grootkloof", withExtension: nil) else {
            throw FixtureError.missing("grootkloof folder in the test bundle")
        }
        return url
    }

    static func url(_ name: String) throws -> URL {
        let url = try directory().appendingPathComponent(name)
        guard FileManager.default.fileExists(atPath: url.path) else { throw FixtureError.missing(name) }
        return url
    }

    /// DEM tile from fixtures/grootkloof/dem/{z}_{x}_{y}.png, if present.
    static func demTile(_ t: TileID) -> Data? {
        guard let dir = try? directory() else { return nil }
        return try? Data(contentsOf: dir.appendingPathComponent("dem").appendingPathComponent("\(t.z)_\(t.x)_\(t.y).png"))
    }

    private static var rendered: (image: CGImage, dpi: Double?)?

    /// grootkloof.pdf rendered once through MapImporter's renderer (shared by tests).
    static func renderedPDF() throws -> (image: CGImage, dpi: Double?) {
        if let rendered { return rendered }
        let (image, dpi) = try MapImporter.renderBaseImage(from: url("grootkloof.pdf"))
        rendered = (image, dpi)
        return (image, dpi)
    }

    /// OCR fixture (bboxes in map.jpg pixels).
    static func ocrFixture() throws -> [OCRObservation] {
        struct File: Decodable { var observations: [OCRObservation] }
        return try JSONDecoder().decode(File.self, from: Data(contentsOf: url("ocr.json"))).observations
    }

    /// Answer `need_dem` from the fixture tiles (absent ones are marked missing) until the core finishes.
    static func georeference(_ session: GeorefSession, _ request: GeorefRequest, maxRounds: Int = 8) throws -> GeorefResponse {
        var response = try session.georeference(request)
        var rounds = 0
        while response.needsDEM, let needed = response.needTiles, !needed.isEmpty, rounds < maxRounds {
            for t in needed {
                if let png = demTile(t), session.addDEMTile(t, png: png) { continue }
                session.markDEMTileMissing(t)
            }
            response = try session.georeference(request)
            rounds += 1
        }
        return response
    }
}

/// Ground truth of the Grootkloof sheet: least-squares affine through the measured graticule
/// (map.jpg pixels, 3507×4960 at 300 dpi → Web Mercator).
struct GrootkloofTruth {
    struct LatLine: Decodable { var y: Double; var lat: Double }
    struct LonLine: Decodable { var x: Double; var lon: Double }

    private struct File: Decodable {
        var imageWidth: Double
        var imageHeight: Double
        var latLines: [LatLine]
        var lonLines: [LonLine]
        var mapFramePx: [Double]

        enum CodingKeys: String, CodingKey {
            case imageWidth = "image_width"
            case imageHeight = "image_height"
            case latLines = "lat_lines"
            case lonLines = "lon_lines"
            case mapFramePx = "map_frame_px"
        }
    }

    var pixelToMercator: Homography
    var imageWidth: Double
    var imageHeight: Double
    var latLines: [LatLine]
    var lonLines: [LonLine]
    /// Map frame [x0, y0, x1, y1] in map.jpg pixels.
    var frame: [Double]

    static func load() throws -> GrootkloofTruth {
        let file = try JSONDecoder().decode(File.self, from: Data(contentsOf: Fixtures.url("truth.json")))
        var src: [SIMD2<Double>] = []
        var dst: [SIMD2<Double>] = []
        for la in file.latLines {
            for lo in file.lonLines {
                src.append(SIMD2(lo.x, la.y))
                dst.append(WebMercator.project(LatLon(lat: la.lat, lon: lo.lon)))
            }
        }
        guard let h = LeastSquares.affine(from: src, to: dst) else { throw FixtureError.missing("truth fit") }
        return GrootkloofTruth(pixelToMercator: h, imageWidth: file.imageWidth, imageHeight: file.imageHeight,
                               latLines: file.latLines, lonLines: file.lonLines, frame: file.mapFramePx)
    }

    var georeference: Georeference {
        Georeference(pixelToMercator: pixelToMercator, method: .grid, confidence: 1, rmsMeters: 1)
    }

    /// Four map-frame corners and the frame centre, in pixels of a raster `k`× map.jpg.
    func checkPoints(scale k: Double) -> [CGPoint] {
        let x0 = frame[0], y0 = frame[1], x1 = frame[2], y1 = frame[3]
        return [(x0, y0), (x1, y0), (x1, y1), (x0, y1), ((x0 + x1) / 2, (y0 + y1) / 2)]
            .map { CGPoint(x: $0.0 * k, y: $0.1 * k) }
    }

    /// Ground distance (m) between where `h` and the truth put pixel `p` of a raster `k`× map.jpg.
    func groundError(of h: Homography, atPixel p: CGPoint, scale k: Double) -> Double {
        let a = h.apply(SIMD2(Double(p.x), Double(p.y)))
        let b = pixelToMercator.apply(SIMD2(Double(p.x) / k, Double(p.y) / k))
        let lat = WebMercator.unproject(b).lat
        let dx = a.x - b.x
        let dy = a.y - b.y
        return (dx * dx + dy * dy).squareRoot() * WebMercator.groundPerMercator(lat: lat)
    }

    func maxGroundError(of h: Homography, scale k: Double) -> Double {
        checkPoints(scale: k).map { groundError(of: h, atPixel: $0, scale: k) }.max() ?? .infinity
    }
}

enum LeastSquares {
    /// Least-squares affine src → dst (6 parameters) as a homography with last row [0, 0, 1].
    static func affine(from src: [SIMD2<Double>], to dst: [SIMD2<Double>]) -> Homography? {
        guard src.count >= 3, src.count == dst.count else { return nil }
        let n = Double(src.count)
        let ms = src.reduce(SIMD2<Double>(0, 0), +) / n
        let md = dst.reduce(SIMD2<Double>(0, 0), +) / n
        var sxx = 0.0, sxy = 0.0, syy = 0.0
        var sxX = 0.0, syX = 0.0, sxY = 0.0, syY = 0.0
        for (s, d) in zip(src, dst) {
            let u = s - ms
            let v = d - md
            sxx += u.x * u.x
            sxy += u.x * u.y
            syy += u.y * u.y
            sxX += u.x * v.x
            syX += u.y * v.x
            sxY += u.x * v.y
            syY += u.y * v.y
        }
        let det = sxx * syy - sxy * sxy
        guard abs(det) > 1e-9 else { return nil }
        let a = (sxX * syy - syX * sxy) / det
        let b = (sxx * syX - sxy * sxX) / det
        let d = (sxY * syy - syY * sxy) / det
        let e = (sxx * syY - sxy * sxY) / det
        let c = md.x - a * ms.x - b * ms.y
        let f = md.y - d * ms.x - e * ms.y
        return Homography([a, b, c, d, e, f, 0, 0, 1])
    }
}

/// Pixels of a CGImage as RGBA8 rows (top row first).
struct RGBAPixels {
    let width: Int
    let height: Int
    let bytes: [UInt8]

    init?(_ image: CGImage) {
        width = image.width
        height = image.height
        var buffer = [UInt8](repeating: 0, count: width * height * 4)
        let ok = buffer.withUnsafeMutableBytes { raw -> Bool in
            guard let space = CGColorSpace(name: CGColorSpace.sRGB),
                  let ctx = CGContext(data: raw.baseAddress, width: image.width, height: image.height, bitsPerComponent: 8,
                                      bytesPerRow: image.width * 4, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.interpolationQuality = .none
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        guard ok else { return nil }
        bytes = buffer
    }

    func rgb(_ x: Int, _ y: Int) -> (r: UInt8, g: UInt8, b: UInt8) {
        let i = (y * width + x) * 4
        return (bytes[i], bytes[i + 1], bytes[i + 2])
    }

    /// Bounding box (top-left origin) of pixels matching `predicate`.
    func bounds(where predicate: (UInt8, UInt8, UInt8) -> Bool) -> CGRect? {
        var minX = Int.max, minY = Int.max, maxX = -1, maxY = -1
        for y in 0..<height {
            for x in 0..<width {
                let p = rgb(x, y)
                if predicate(p.r, p.g, p.b) {
                    minX = min(minX, x)
                    minY = min(minY, y)
                    maxX = max(maxX, x)
                    maxY = max(maxY, y)
                }
            }
        }
        guard maxX >= 0 else { return nil }
        return CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)
    }
}

/// Fresh empty directory under the test's temporary directory.
func makeTemporaryDirectory(_ name: String = UUID().uuidString) throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("MapMeTests-\(name)", isDirectory: true)
    try? FileManager.default.removeItem(at: url)
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}
