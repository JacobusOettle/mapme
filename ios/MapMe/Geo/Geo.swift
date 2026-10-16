import CoreGraphics
import CoreLocation
import Foundation

// Shared geodesy and transform math. Conventions (see docs/ARCHITECTURE.md):
// - Image pixels: top-left origin, x right, y down, units = base-image pixels.
// - Ground frame: Web Mercator metres, +y = north.
// - Georeference: 3×3 row-major homography pixel → Mercator.

struct LatLon: Codable, Hashable, Sendable {
    var lat: Double
    var lon: Double

    init(lat: Double, lon: Double) {
        self.lat = lat
        self.lon = lon
    }

    init(_ c: CLLocationCoordinate2D) {
        self.lat = c.latitude
        self.lon = c.longitude
    }

    var coordinate: CLLocationCoordinate2D { CLLocationCoordinate2D(latitude: lat, longitude: lon) }
}

/// Web Mercator (EPSG:3857) projection.
enum WebMercator {
    static let radius = 6_378_137.0

    static func project(_ p: LatLon) -> SIMD2<Double> {
        let lat = min(max(p.lat, -85.05112878), 85.05112878)
        let x = radius * p.lon * .pi / 180
        let y = radius * log(tan(.pi / 4 + lat * .pi / 360))
        return SIMD2(x, y)
    }

    static func unproject(_ m: SIMD2<Double>) -> LatLon {
        let lon = m.x / radius * 180 / .pi
        let lat = (2 * atan(exp(m.y / radius)) - .pi / 2) * 180 / .pi
        return LatLon(lat: lat, lon: lon)
    }

    /// Ground metres per Mercator metre at a latitude.
    static func groundPerMercator(lat: Double) -> Double { cos(lat * .pi / 180) }
}

/// 3×3 projective transform, row-major: [X, Y, W] = M · [x, y, 1].
struct Homography: Codable, Hashable {
    var m: [Double]

    init(_ m: [Double]) {
        precondition(m.count == 9, "Homography needs 9 values")
        self.m = m
    }

    static let identity = Homography([1, 0, 0, 0, 1, 0, 0, 0, 1])

    func apply(_ p: SIMD2<Double>) -> SIMD2<Double> {
        let w = m[6] * p.x + m[7] * p.y + m[8]
        return SIMD2((m[0] * p.x + m[1] * p.y + m[2]) / w, (m[3] * p.x + m[4] * p.y + m[5]) / w)
    }

    var inverse: Homography? {
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6])
        guard det.isFinite, abs(det) > 1e-300 else { return nil }
        return Homography([
            (m[4] * m[8] - m[5] * m[7]) / det,
            (m[2] * m[7] - m[1] * m[8]) / det,
            (m[1] * m[5] - m[2] * m[4]) / det,
            (m[5] * m[6] - m[3] * m[8]) / det,
            (m[0] * m[8] - m[2] * m[6]) / det,
            (m[2] * m[3] - m[0] * m[5]) / det,
            (m[3] * m[7] - m[4] * m[6]) / det,
            (m[1] * m[6] - m[0] * m[7]) / det,
            (m[0] * m[4] - m[1] * m[3]) / det,
        ])
    }
}

enum GeorefMethod: String, Codable, Hashable {
    case grid          // grid / graticule lines + labels
    case labels        // labels only (coarse)
    case contours      // contour lines matched against terrain
    case manual        // user control points
}

/// A smooth correction for paper that does not lie flat (fold creases, curl) in photo mode
/// (docs/ARCHITECTURE.md, "warp" in the georeference response): Web-Mercator offsets on a regular
/// lattice of nodes over the image, interpolated bilinearly. The full model is
/// `merc(p) = H(p) + B(p)` with `H` the georeference's homography and `B` this warp.
///
/// Node `(i, j)` (column `i`, row `j`) sits at pixel `origin + (i·spacing.x, j·spacing.y)`; its
/// offset is `offsets[2·(j·cols + i)]` (x) and `offsets[2·(j·cols + i) + 1]` (y), in Mercator
/// metres. Beyond the lattice the warp is constant (lattice coordinates are clamped).
struct MeshWarp: Codable, Hashable {
    var cols: Int
    var rows: Int
    /// Pixel position of node (0, 0).
    var origin: SIMD2<Double>
    /// Pixels between neighbouring nodes along x and y.
    var spacing: SIMD2<Double>
    /// Interleaved (dx, dy) per node, row by row: `2 · cols · rows` values.
    var offsets: [Double]

    enum CodingKeys: String, CodingKey {
        case cols, rows, origin, spacing
        case offsets = "offsets_m"
    }

    init(cols: Int, rows: Int, origin: SIMD2<Double>, spacing: SIMD2<Double>, offsets: [Double]) {
        self.cols = cols
        self.rows = rows
        self.origin = origin
        self.spacing = spacing
        self.offsets = offsets
    }

    /// A `cols` × `rows` lattice covering a `width` × `height` image (the core's layout: origin
    /// (0, 0), first and last nodes on the image edges).
    init(cols: Int, rows: Int, width: Double, height: Double, offsets: [Double]) {
        self.init(cols: cols, rows: rows, origin: SIMD2(0, 0),
                  spacing: SIMD2(width / Double(max(cols - 1, 1)), height / Double(max(rows - 1, 1))),
                  offsets: offsets)
    }

    /// Well-formed: at least one node, one offset pair per node, finite positive spacing and
    /// finite values. A malformed warp is ignored (zero offset) rather than trusted.
    var isValid: Bool {
        guard cols >= 1, rows >= 1, offsets.count == 2 * cols * rows else { return false }
        guard origin.x.isFinite, origin.y.isFinite, spacing.x.isFinite, spacing.y.isFinite else { return false }
        guard spacing.x > 0, spacing.y > 0 else { return false }
        return offsets.allSatisfy { $0.isFinite }
    }

    /// Pixel position of node (i, j).
    func nodePixel(_ i: Int, _ j: Int) -> SIMD2<Double> {
        SIMD2(origin.x + Double(i) * spacing.x, origin.y + Double(j) * spacing.y)
    }

    /// Offset of node (i, j) in Mercator metres.
    func nodeOffset(_ i: Int, _ j: Int) -> SIMD2<Double> {
        let k = 2 * (j * cols + i)
        return SIMD2(offsets[k], offsets[k + 1])
    }

    /// `B(p)`: the offset in Mercator metres at a pixel (zero for a malformed warp).
    func offset(atPixel p: CGPoint) -> SIMD2<Double> {
        offset(at: SIMD2(Double(p.x), Double(p.y)))
    }

    /// `B(p)` for a pixel given as a vector.
    func offset(at p: SIMD2<Double>) -> SIMD2<Double> {
        // Cheap structural checks only (this runs for every projected point); `isValid` also
        // scans the values and is checked where a warp enters the app.
        let zero = SIMD2<Double>(0, 0)
        guard cols >= 1, rows >= 1, offsets.count == 2 * cols * rows else { return zero }
        guard spacing.x > 0, spacing.y > 0, p.x.isFinite, p.y.isFinite else { return zero }
        let maxU = Double(cols - 1)
        let maxV = Double(rows - 1)
        let u: Double = min(max((p.x - origin.x) / spacing.x, 0), maxU)
        let v: Double = min(max((p.y - origin.y) / spacing.y, 0), maxV)
        guard u.isFinite, v.isFinite else { return zero }
        let i0 = min(Int(u.rounded(.down)), max(cols - 2, 0))
        let j0 = min(Int(v.rounded(.down)), max(rows - 2, 0))
        let i1 = min(i0 + 1, cols - 1)
        let j1 = min(j0 + 1, rows - 1)
        let fu: Double = u - Double(i0)
        let fv: Double = v - Double(j0)
        let gu: Double = 1 - fu
        let gv: Double = 1 - fv
        let top: SIMD2<Double> = nodeOffset(i0, j0) * gu + nodeOffset(i1, j0) * fu
        let bottom: SIMD2<Double> = nodeOffset(i0, j1) * gu + nodeOffset(i1, j1) * fu
        return top * gv + bottom * fv
    }

    /// Largest node offset (Mercator metres).
    var maxOffset: Double {
        var best = 0.0
        var k = 0
        while k + 1 < offsets.count {
            best = max(best, (offsets[k] * offsets[k] + offsets[k + 1] * offsets[k + 1]).squareRoot())
            k += 2
        }
        return best
    }

    /// This warp seen through a change of pixels: a `cols` × `rows` lattice over a new
    /// `width` × `height` image whose node offsets are this warp's offsets at `toSelf(node)`,
    /// where `toSelf` maps the new image's pixels to this warp's pixels.
    func resampled(cols: Int, rows: Int, width: Double, height: Double,
                   toSelf: (SIMD2<Double>) -> SIMD2<Double>) -> MeshWarp {
        var lattice = MeshWarp(cols: cols, rows: rows, width: width, height: height, offsets: [])
        var values: [Double] = []
        values.reserveCapacity(2 * cols * rows)
        for j in 0..<max(rows, 0) {
            for i in 0..<max(cols, 0) {
                let b = offset(at: toSelf(lattice.nodePixel(i, j)))
                values.append(b.x)
                values.append(b.y)
            }
        }
        lattice.offsets = values
        return lattice
    }
}

/// Where a map sits on Earth.
struct Georeference: Codable, Hashable {
    var pixelToMercator: Homography
    var method: GeorefMethod
    /// 0…1
    var confidence: Double
    /// Estimated position error in ground metres.
    var rmsMeters: Double?
    var createdAt: Date
    /// Photo mode: the correction for paper that does not lie flat, added to `pixelToMercator`
    /// (`merc = H(p) + B(p)`). Nil for flat maps, manual calibrations and documents saved before
    /// warps existed (an absent key decodes as nil).
    var warp: MeshWarp?

    /// Most fixed-point iterations of the inverse with a warp. The core keeps the warp's
    /// derivative below 0.35× that of `H`, so each step shrinks the error at least ~3×; smooth
    /// real warps converge in a few steps and the loop stops once a step is below 1e-6 px.
    static let inverseIterations = 12

    init(pixelToMercator: Homography, method: GeorefMethod, confidence: Double, rmsMeters: Double?,
         createdAt: Date = Date(), warp: MeshWarp? = nil) {
        self.pixelToMercator = pixelToMercator
        self.method = method
        self.confidence = confidence
        self.rmsMeters = rmsMeters
        self.createdAt = createdAt
        self.warp = warp
    }

    /// Inverse of the homography alone (no warp): the flat model. Use `pixel(for:)` or a
    /// `Projector` to place coordinates on the map.
    var mercatorToPixel: Homography { pixelToMercator.inverse ?? .identity }

    /// Web-Mercator position of a pixel: `H(p) + B(p)`.
    func mercator(forPixel p: SIMD2<Double>) -> SIMD2<Double> {
        let m = pixelToMercator.apply(p)
        guard let warp else { return m }
        return m + warp.offset(at: p)
    }

    /// Pixel of a Web-Mercator position, with `inverse` = `mercatorToPixel` (passed in so callers
    /// projecting many points work it out once). With a warp: `p₀ = H⁻¹(m)`,
    /// `pₖ₊₁ = H⁻¹(m − B(pₖ))`.
    func pixel(forMercator m: SIMD2<Double>, inverse: Homography) -> SIMD2<Double> {
        let p0 = inverse.apply(m)
        guard let warp, p0.x.isFinite, p0.y.isFinite else { return p0 }
        var p = p0
        for _ in 0..<Self.inverseIterations {
            let next = inverse.apply(m - warp.offset(at: p))
            guard next.x.isFinite, next.y.isFinite else { return p0 }
            let step = next - p
            p = next
            if step.x * step.x + step.y * step.y < 1e-12 { break }
        }
        return p
    }

    /// Map pixel of a coordinate.
    func pixel(for p: LatLon) -> CGPoint {
        let px = pixel(forMercator: WebMercator.project(p), inverse: mercatorToPixel)
        return CGPoint(x: px.x, y: px.y)
    }

    /// Coordinate of a map pixel.
    func latLon(forPixel p: CGPoint) -> LatLon {
        WebMercator.unproject(mercator(forPixel: SIMD2(Double(p.x), Double(p.y))))
    }

    /// Direction of true north at a pixel, as an angle in radians measured clockwise
    /// from image-up (0 = north is straight up on the image).
    func northAngle(atPixel p: CGPoint) -> Double {
        let ll = latLon(forPixel: p)
        let here = SIMD2(Double(p.x), Double(p.y))
        let north = pixel(forMercator: WebMercator.project(LatLon(lat: ll.lat + 0.001, lon: ll.lon)),
                          inverse: mercatorToPixel)
        let d = north - here
        // Image-up is (0,-1). Clockwise angle from up to d.
        return atan2(d.x, -d.y)
    }

    /// Ground metres per pixel at a pixel.
    func metersPerPixel(atPixel p: CGPoint) -> Double {
        let a = latLon(forPixel: p)
        let b = latLon(forPixel: CGPoint(x: p.x + 1, y: p.y))
        let c = latLon(forPixel: CGPoint(x: p.x, y: p.y + 1))
        return (Geodesy.distance(a, b) + Geodesy.distance(a, c)) / 2
    }

    /// Coordinates → map pixels for many points (track, POIs, the puck): the inverse homography is
    /// worked out once; the warp, when there is one, is applied per point.
    struct Projector {
        let georef: Georeference
        private let inverse: Homography

        init(_ georef: Georeference) {
            self.georef = georef
            inverse = georef.mercatorToPixel
        }

        func pixel(for p: LatLon) -> CGPoint {
            let px = georef.pixel(forMercator: WebMercator.project(p), inverse: inverse)
            return CGPoint(x: px.x, y: px.y)
        }
    }
}

enum Geodesy {
    static let meanRadius = 6_371_008.8

    static func distance(_ a: LatLon, _ b: LatLon) -> Double {
        let p1 = a.lat * .pi / 180, p2 = b.lat * .pi / 180
        let dp = p2 - p1
        let dl = (b.lon - a.lon) * .pi / 180
        let h = sin(dp / 2) * sin(dp / 2) + cos(p1) * cos(p2) * sin(dl / 2) * sin(dl / 2)
        return 2 * meanRadius * asin(min(1, sqrt(h)))
    }

    /// Initial bearing a → b in degrees clockwise from true north, 0..<360.
    static func bearing(_ a: LatLon, _ b: LatLon) -> Double {
        let p1 = a.lat * .pi / 180, p2 = b.lat * .pi / 180
        let dl = (b.lon - a.lon) * .pi / 180
        let y = sin(dl) * cos(p2)
        let x = cos(p1) * sin(p2) - sin(p1) * cos(p2) * cos(dl)
        let deg = atan2(y, x) * 180 / .pi
        return (deg + 360).truncatingRemainder(dividingBy: 360)
    }

    static func destination(_ a: LatLon, bearing: Double, distance: Double) -> LatLon {
        let d = distance / meanRadius
        let b = bearing * .pi / 180
        let p1 = a.lat * .pi / 180
        let l1 = a.lon * .pi / 180
        let p2 = asin(sin(p1) * cos(d) + cos(p1) * sin(d) * cos(b))
        let l2 = l1 + atan2(sin(b) * sin(d) * cos(p1), cos(d) - sin(p1) * sin(p2))
        var lon = l2 * 180 / .pi
        lon = (lon + 540).truncatingRemainder(dividingBy: 360) - 180
        return LatLon(lat: p2 * 180 / .pi, lon: lon)
    }

    /// Compass point name for a bearing ("N", "NE", …).
    static func compassPoint(_ bearing: Double) -> String {
        let names = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"]
        let i = Int((bearing / 22.5).rounded()) % 16
        return names[(i + 16) % 16]
    }
}

enum DistanceUnits: String, Codable, CaseIterable, Identifiable {
    case metric, imperial
    var id: String { rawValue }
}

enum CoordinateFormat: String, Codable, CaseIterable, Identifiable {
    case decimal, dms, utm
    var id: String { rawValue }

    var label: String {
        switch self {
        case .decimal: return "Decimal degrees"
        case .dms: return "Degrees, minutes, seconds"
        case .utm: return "UTM"
        }
    }
}

enum GeoFormat {
    static func distance(_ meters: Double, units: DistanceUnits) -> String {
        switch units {
        case .metric:
            if meters < 1000 { return "\(Int(meters.rounded())) m" }
            return String(format: meters < 10_000 ? "%.2f km" : "%.1f km", meters / 1000)
        case .imperial:
            let feet = meters * 3.28084
            if feet < 1000 { return "\(Int(feet.rounded())) ft" }
            return String(format: "%.2f mi", meters / 1609.344)
        }
    }

    static func elevation(_ meters: Double, units: DistanceUnits) -> String {
        switch units {
        case .metric: return "\(Int(meters.rounded())) m"
        case .imperial: return "\(Int((meters * 3.28084).rounded())) ft"
        }
    }

    static func speed(_ mps: Double, units: DistanceUnits) -> String {
        switch units {
        case .metric: return String(format: "%.1f km/h", mps * 3.6)
        case .imperial: return String(format: "%.1f mph", mps * 2.236936)
        }
    }

    static func duration(_ seconds: TimeInterval) -> String {
        let s = Int(seconds)
        if s >= 3600 { return String(format: "%d:%02d:%02d", s / 3600, (s % 3600) / 60, s % 60) }
        return String(format: "%d:%02d", s / 60, s % 60)
    }

    static func coordinate(_ p: LatLon, format: CoordinateFormat) -> String {
        switch format {
        case .decimal:
            return String(format: "%.5f, %.5f", p.lat, p.lon)
        case .dms:
            return "\(dms(p.lat, pos: "N", neg: "S"))  \(dms(p.lon, pos: "E", neg: "W"))"
        case .utm:
            let u = UTM.from(p)
            return String(format: "%d%@ %.0fE %.0fN", u.zone, u.band, u.easting, u.northing)
        }
    }

    private static func dms(_ v: Double, pos: String, neg: String) -> String {
        let a = abs(v)
        var d = Int(a)
        var m = Int((a - Double(d)) * 60)
        var s = ((a - Double(d)) * 60 - Double(m)) * 60
        if s >= 59.95 { s = 0; m += 1 }
        if m >= 60 { m = 0; d += 1 }
        return String(format: "%d°%02d′%04.1f″%@", d, m, s, v >= 0 ? pos : neg)
    }
}

/// UTM (WGS84) forward projection for coordinate display.
struct UTM {
    var zone: Int
    var band: String
    var easting: Double
    var northing: Double

    static func from(_ p: LatLon) -> UTM {
        let a = 6_378_137.0, f = 1 / 298.257223563, k0 = 0.9996
        let e2 = f * (2 - f), ep2 = e2 / (1 - e2)
        var zone = Int(floor((p.lon + 180) / 6)) + 1
        if p.lat >= 56, p.lat < 64, p.lon >= 3, p.lon < 12 { zone = 32 }
        if p.lat >= 72, p.lat < 84 {
            if p.lon >= 0, p.lon < 9 { zone = 31 } else if p.lon >= 9, p.lon < 21 { zone = 33 } else if p.lon >= 21, p.lon < 33 { zone = 35 } else if p.lon >= 33, p.lon < 42 { zone = 37 }
        }
        zone = min(max(zone, 1), 60)
        let phi = p.lat * .pi / 180
        let lon0 = (Double(zone - 1) * 6 - 180 + 3) * .pi / 180
        let n = a / sqrt(1 - e2 * sin(phi) * sin(phi))
        let t = tan(phi) * tan(phi)
        let c = ep2 * cos(phi) * cos(phi)
        let aa = cos(phi) * (p.lon * .pi / 180 - lon0)
        let e4 = e2 * e2, e6 = e4 * e2
        let m = a * ((1 - e2 / 4 - 3 * e4 / 64 - 5 * e6 / 256) * phi
            - (3 * e2 / 8 + 3 * e4 / 32 + 45 * e6 / 1024) * sin(2 * phi)
            + (15 * e4 / 256 + 45 * e6 / 1024) * sin(4 * phi)
            - (35 * e6 / 3072) * sin(6 * phi))
        let x = k0 * n * (aa + (1 - t + c) * pow(aa, 3) / 6 + (5 - 18 * t + t * t + 72 * c - 58 * ep2) * pow(aa, 5) / 120)
        var y = k0 * (m + n * tan(phi) * (aa * aa / 2 + (5 - t + 9 * c + 4 * c * c) * pow(aa, 4) / 24
            + (61 - 58 * t + t * t + 600 * c - 330 * ep2) * pow(aa, 6) / 720))
        if p.lat < 0 { y += 10_000_000 }
        let bands = Array("CDEFGHJKLMNPQRSTUVWXX")
        let bi = min(max(Int(floor((p.lat + 80) / 8)), 0), bands.count - 1)
        return UTM(zone: zone, band: String(bands[bi]), easting: x + 500_000, northing: y)
    }
}
