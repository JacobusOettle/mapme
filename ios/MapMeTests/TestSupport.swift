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

    /// fixtures/grootkloof/photo/`name` (the real phone photo of the printed sheet and its truth).
    static func photoURL(_ name: String) throws -> URL {
        let url = try directory().appendingPathComponent("photo").appendingPathComponent(name)
        guard FileManager.default.fileExists(atPath: url.path) else { throw FixtureError.missing("photo/" + name) }
        return url
    }

    /// The fixture OCR moved into the real photo's pixels (photo/ocr.json).
    static func photoOCRFixture() throws -> [PhotoOCRBox] {
        struct File: Decodable { var observations: [PhotoOCRBox] }
        return try JSONDecoder().decode(File.self, from: Data(contentsOf: photoURL("ocr.json"))).observations
    }

    /// A DEMService that serves the fixture tiles (copied into a fresh cache in its z/x/y.png
    /// layout) and never touches the network: tiles the fixtures lack come back as unavailable, so
    /// the core is told they are missing, as `georeference(_:_:)` does.
    static func offlineDEMService() throws -> DEMService {
        let cache = try makeTemporaryDirectory("dem-fixtures-\(UUID().uuidString)")
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [NotFoundURLProtocol.self]
        let service = DEMService(cacheDirectory: cache, session: URLSession(configuration: config))
        let source = try directory().appendingPathComponent("dem")
        let fm = FileManager.default
        for name in try fm.contentsOfDirectory(atPath: source.path) where name.hasSuffix(".png") {
            let parts = name.dropLast(4).split(separator: "_")
            guard parts.count == 3, let z = Int(parts[0]), let x = Int(parts[1]), let y = Int(parts[2]) else { continue }
            let target = service.cacheURL(for: TileID(z: z, x: x, y: y))
            try fm.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            try fm.copyItem(at: source.appendingPathComponent(name), to: target)
        }
        return service
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

/// Ground truth of the real phone photo of the printed sheet (fixtures/grootkloof/photo/truth.json,
/// photo pixels, top-left origin): the paper's and the map frame's corners and 670 control points
/// photo pixel → lat/lon measured through a dense SIFT field (the paper is folded, so no single
/// homography fits it; `homographyFloor` is how well the best one does).
struct RealPhotoTruth: Decodable {
    struct ControlPoint: Decodable {
        var x: Double
        var y: Double
        var lat: Double
        var lon: Double
    }

    struct ErrorStats: Codable {
        var rms: Double?
        var median: Double
        var p90: Double
        var max: Double
    }

    var image: String
    var imageWidth: Int
    var imageHeight: Int
    /// The sheet of paper, TL, TR, BR, BL.
    var pageQuadPoints: [[Double]]
    /// The map's neatline, TL, TR, BR, BL.
    var frameQuadPoints: [[Double]]
    var controlPoints: [ControlPoint]
    var homographyFloor: ErrorStats?

    enum CodingKeys: String, CodingKey {
        case image
        case imageWidth = "image_width"
        case imageHeight = "image_height"
        case pageQuadPoints = "page_quad"
        case frameQuadPoints = "frame_quad"
        case controlPoints = "control_points"
        case homographyFloor = "homography_floor_m"
    }

    static func load() throws -> RealPhotoTruth {
        try JSONDecoder().decode(RealPhotoTruth.self, from: Data(contentsOf: Fixtures.photoURL("truth.json")))
    }

    var pageQuad: PhotoQuad? { PhotoQuad(json: pageQuadPoints) }
    var frameQuad: PhotoQuad? { PhotoQuad(json: frameQuadPoints) }

    /// Inside (or on) a convex quad ordered clockwise on screen, like the truth's quads.
    static func isInside(_ p: CGPoint, _ quad: PhotoQuad) -> Bool {
        let corners = quad.points
        for i in 0..<4 {
            let a = corners[i]
            let b = corners[(i + 1) % 4]
            let ex = Double(b.x - a.x)
            let ey = Double(b.y - a.y)
            let px = Double(p.x - a.x)
            let py = Double(p.y - a.y)
            if ex * py - ey * px < 0 { return false }
        }
        return true
    }

    /// Control points inside the map frame.
    var framedControlPoints: [ControlPoint] {
        guard let frame = frameQuad else { return controlPoints }
        return controlPoints.filter { Self.isInside(CGPoint(x: $0.x, y: $0.y), frame) }
    }
}

/// One box of the fixture OCR in the real photo's pixels (photo/ocr.json).
struct PhotoOCRBox: Decodable {
    var text: String
    /// The box's corners TL, TR, BR, BL in photo pixels.
    var quad: [[Double]]
    /// Their axis-aligned extent [x, y, w, h].
    var bbox: [Double]
    var confidence: Float

    /// The observation in the pixels of the image `h` maps the photo to (e.g. the flattened page):
    /// the axis-aligned extent of the box's mapped corners.
    func observation(through h: Homography) -> OCRObservation {
        let corners = quad.filter { $0.count == 2 }.map { h.apply(SIMD2($0[0], $0[1])) }
        let xs = corners.map(\.x)
        let ys = corners.map(\.y)
        guard corners.count == 4, let x0 = xs.min(), let x1 = xs.max(), let y0 = ys.min(), let y1 = ys.max() else {
            return OCRObservation(text: text, bbox: bbox, confidence: confidence)
        }
        return OCRObservation(text: text, bbox: [x0, y0, x1 - x0, y1 - y0], confidence: confidence)
    }
}

/// Median, 90th percentile and maximum of a set of errors (linear interpolation between ranks, as
/// numpy's default).
struct ErrorSummary: Codable {
    var count: Int
    var median: Double
    var p90: Double
    var max: Double

    init(_ values: [Double]) {
        let sorted = values.sorted()
        count = sorted.count
        median = Self.percentile(sorted, 0.5)
        p90 = Self.percentile(sorted, 0.9)
        max = sorted.last ?? .nan
    }

    static func percentile(_ sorted: [Double], _ q: Double) -> Double {
        guard !sorted.isEmpty else { return .nan }
        let position = q * Double(sorted.count - 1)
        let lower = Int(position.rounded(.down))
        let upper = Swift.min(lower + 1, sorted.count - 1)
        let f = position - Double(lower)
        return sorted[lower] * (1 - f) + sorted[upper] * f
    }

    var text: String { String(format: "median %.1f m, p90 %.1f m, max %.1f m (%ld points)", median, p90, max, count) }
}

/// Answers every request with 404, so a test never depends on the network.
final class NotFoundURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let url = request.url ?? URL(fileURLWithPath: "/")
        if let response = HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: nil) {
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        }
        client?.urlProtocol(self, didLoad: Data())
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
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
