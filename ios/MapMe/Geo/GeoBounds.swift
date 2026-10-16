import CoreGraphics
import Foundation

/// A latitude/longitude box (degrees). `west > east` means it crosses the antimeridian (180°).
/// Offline regions, the world view's footprints and geo item queries use it.
struct GeoBounds: Codable, Hashable, Sendable {
    var south: Double
    var west: Double
    var north: Double
    var east: Double

    init(south: Double, west: Double, north: Double, east: Double) {
        self.south = south
        self.west = west
        self.north = north
        self.east = east
    }

    /// The smallest box holding the points (nil for none). Longitudes are taken as given: a set
    /// of points spanning the antimeridian gives a box around the world the other way.
    static func enclosing(_ points: [LatLon]) -> GeoBounds? {
        guard let first = points.first else { return nil }
        var b = GeoBounds(south: first.lat, west: first.lon, north: first.lat, east: first.lon)
        for p in points.dropFirst() {
            b.south = min(b.south, p.lat)
            b.north = max(b.north, p.lat)
            b.west = min(b.west, p.lon)
            b.east = max(b.east, p.lon)
        }
        return b
    }

    var crossesAntimeridian: Bool { west > east }

    /// Width in degrees of longitude (across the antimeridian when it crosses it).
    var longitudeSpan: Double { crossesAntimeridian ? east + 360 - west : east - west }

    var center: LatLon {
        var lon = west + longitudeSpan / 2
        if lon > 180 { lon -= 360 }
        return LatLon(lat: (south + north) / 2, lon: lon)
    }

    func contains(_ p: LatLon) -> Bool {
        guard p.lat >= south, p.lat <= north else { return false }
        return crossesAntimeridian ? (p.lon >= west || p.lon <= east) : (p.lon >= west && p.lon <= east)
    }

    func intersects(_ other: GeoBounds) -> Bool {
        guard other.south <= north, other.north >= south else { return false }
        // Compare as intervals on the circle: unwrap both from this box's west edge.
        func unwrapped(_ lon: Double) -> Double {
            var d = lon - west
            while d < 0 { d += 360 }
            while d >= 360 { d -= 360 }
            return d
        }
        let span = longitudeSpan
        let otherStart = unwrapped(other.west)
        let otherSpan = other.longitudeSpan
        if otherStart <= span { return true }
        return otherStart + otherSpan >= 360
    }

    /// Grown by `meters` on every side (latitudes clamped to ±85°, the Web Mercator limit).
    func expanded(byMeters meters: Double) -> GeoBounds {
        let dLat = meters / 111_320
        let cosLat = max(0.01, cos(max(abs(south), abs(north)) * .pi / 180))
        let dLon = min(180, meters / (111_320 * cosLat))
        var w = west - dLon
        var e = east + dLon
        if longitudeSpan + 2 * dLon >= 360 {
            w = -180
            e = 180
        } else {
            if w < -180 { w += 360 }
            if e > 180 { e -= 360 }
        }
        return GeoBounds(south: max(-85, south - dLat), west: w, north: min(85, north + dLat), east: e)
    }

    /// `[south, west, north, east]`, as `mm_tile_plan`'s `"bbox"`.
    var array: [Double] { [south, west, north, east] }
}

/// An axis-aligned rectangle in Web Mercator metres (+y north).
struct MercatorRect: Codable, Hashable, Sendable {
    var minX: Double
    var minY: Double
    var maxX: Double
    var maxY: Double

    init(minX: Double, minY: Double, maxX: Double, maxY: Double) {
        self.minX = minX
        self.minY = minY
        self.maxX = maxX
        self.maxY = maxY
    }

    /// The rectangle holding these Mercator points (nil for none).
    init?(enclosing points: [SIMD2<Double>]) {
        guard let first = points.first else { return nil }
        var r = MercatorRect(minX: first.x, minY: first.y, maxX: first.x, maxY: first.y)
        for p in points.dropFirst() {
            r.minX = min(r.minX, p.x)
            r.minY = min(r.minY, p.y)
            r.maxX = max(r.maxX, p.x)
            r.maxY = max(r.maxY, p.y)
        }
        self = r
    }

    /// A box that does not cross the antimeridian.
    init(_ bounds: GeoBounds) {
        let sw = WebMercator.project(LatLon(lat: bounds.south, lon: bounds.west))
        let ne = WebMercator.project(LatLon(lat: bounds.north, lon: bounds.east))
        self.init(minX: sw.x, minY: sw.y, maxX: ne.x, maxY: ne.y)
    }

    var width: Double { maxX - minX }
    var height: Double { maxY - minY }
    var center: SIMD2<Double> { SIMD2((minX + maxX) / 2, (minY + maxY) / 2) }

    func contains(_ p: SIMD2<Double>) -> Bool {
        p.x >= minX && p.x <= maxX && p.y >= minY && p.y <= maxY
    }

    func expanded(by m: Double) -> MercatorRect {
        MercatorRect(minX: minX - m, minY: minY - m, maxX: maxX + m, maxY: maxY + m)
    }

    /// `[min_x, min_y, max_x, max_y]`, as the terrain requests' `"rect"`.
    var array: [Double] { [minX, minY, maxX, maxY] }
}

/// Slippy-map tile math in Web Mercator (the DEM's XYZ tiling, 256 px tiles), as the core's
/// `dem.rs` does it.
enum TileMath {
    /// Half the Mercator world's width, metres.
    static let halfWorld = Double.pi * WebMercator.radius

    /// Mercator metres per tile pixel at a zoom.
    static func metersPerPixel(zoom: Int) -> Double {
        2 * halfWorld / (256 * Double(1 << max(0, zoom)))
    }

    /// Fractional tile coordinates of a Mercator point.
    static func tileCoordinates(_ m: SIMD2<Double>, zoom: Int) -> SIMD2<Double> {
        let n = Double(1 << max(0, zoom))
        return SIMD2((m.x + halfWorld) / (2 * halfWorld) * n, (halfWorld - m.y) / (2 * halfWorld) * n)
    }

    /// The tile holding a coordinate.
    static func tile(for p: LatLon, zoom: Int) -> TileID {
        let t = tileCoordinates(WebMercator.project(p), zoom: zoom)
        let n = 1 << max(0, zoom)
        guard t.x.isFinite, t.y.isFinite else { return TileID(z: zoom, x: 0, y: 0) }
        return TileID(z: zoom, x: min(max(Int(t.x.rounded(.down)), 0), n - 1), y: min(max(Int(t.y.rounded(.down)), 0), n - 1))
    }

    /// A tile's Mercator bounds.
    static func bounds(of t: TileID) -> MercatorRect {
        let size = 2 * halfWorld / Double(1 << max(0, t.z))
        let minX = -halfWorld + Double(t.x) * size
        let maxY = halfWorld - Double(t.y) * size
        return MercatorRect(minX: minX, minY: maxY - size, maxX: minX + size, maxY: maxY)
    }

    /// Tiles of a zoom intersecting a Mercator rectangle (clamped to the world), row by row.
    static func tiles(covering r: MercatorRect, zoom: Int) -> [TileID] {
        let n = 1 << max(0, zoom)
        let a = tileCoordinates(SIMD2(r.minX, r.maxY), zoom: zoom)
        let b = tileCoordinates(SIMD2(r.maxX, r.minY), zoom: zoom)
        guard a.x.isFinite, a.y.isFinite, b.x.isFinite, b.y.isFinite else { return [] }
        let x0 = min(max(Int(a.x.rounded(.down)), 0), n - 1)
        let x1 = min(max(Int(b.x.rounded(.down)), 0), n - 1)
        let y0 = min(max(Int(a.y.rounded(.down)), 0), n - 1)
        let y1 = min(max(Int(b.y.rounded(.down)), 0), n - 1)
        guard x0 <= x1, y0 <= y1 else { return [] }
        var out: [TileID] = []
        out.reserveCapacity((x1 - x0 + 1) * (y1 - y0 + 1))
        for y in y0...y1 {
            for x in x0...x1 {
                out.append(TileID(z: zoom, x: x, y: y))
            }
        }
        return out
    }
}
