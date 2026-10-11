import CoreGraphics
import CoreLocation
import Foundation

// Shared geodesy and transform math. Conventions (see docs/ARCHITECTURE.md):
// - Image pixels: top-left origin, x right, y down, units = base-image pixels.
// - Ground frame: Web Mercator metres, +y = north.
// - Georeference: 3×3 row-major homography pixel → Mercator.

struct LatLon: Codable, Hashable {
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

/// Where a map sits on Earth.
struct Georeference: Codable, Hashable {
    var pixelToMercator: Homography
    var method: GeorefMethod
    /// 0…1
    var confidence: Double
    /// Estimated position error in ground metres.
    var rmsMeters: Double?
    var createdAt: Date

    init(pixelToMercator: Homography, method: GeorefMethod, confidence: Double, rmsMeters: Double?, createdAt: Date = Date()) {
        self.pixelToMercator = pixelToMercator
        self.method = method
        self.confidence = confidence
        self.rmsMeters = rmsMeters
        self.createdAt = createdAt
    }

    var mercatorToPixel: Homography { pixelToMercator.inverse ?? .identity }

    /// Map pixel of a coordinate.
    func pixel(for p: LatLon) -> CGPoint {
        let px = mercatorToPixel.apply(WebMercator.project(p))
        return CGPoint(x: px.x, y: px.y)
    }

    /// Coordinate of a map pixel.
    func latLon(forPixel p: CGPoint) -> LatLon {
        WebMercator.unproject(pixelToMercator.apply(SIMD2(Double(p.x), Double(p.y))))
    }

    /// Direction of true north at a pixel, as an angle in radians measured clockwise
    /// from image-up (0 = north is straight up on the image).
    func northAngle(atPixel p: CGPoint) -> Double {
        let ll = latLon(forPixel: p)
        let here = SIMD2(Double(p.x), Double(p.y))
        let north = mercatorToPixel.apply(WebMercator.project(LatLon(lat: ll.lat + 0.001, lon: ll.lon)))
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
