import CoreGraphics
import Foundation

/// Where a position falls on a route line.
struct RouteMatch: Equatable, Sendable {
    /// Metres along the line from its start to the nearest point.
    var distanceAlongM: Double
    /// Metres from the position to that point.
    var distanceFromLineM: Double
    /// The segment it is on (points `segment` and `segment + 1`).
    var segment: Int
    /// The nearest point.
    var point: LatLon
}

/// A route line prepared for many queries (following, the profile's scrub point): cumulative
/// distances and fast nearest-point searches in local metres. Value type, safe on any thread.
struct RouteLine: Sendable {
    let points: [LatLon]
    /// Metres along the line at each point (haversine on the mean Earth radius, as the core).
    let cumulative: [Double]

    var length: Double { cumulative.last ?? 0 }

    init(_ points: [LatLon]) {
        self.points = points
        cumulative = RouteGeometry.cumulativeDistances(points)
    }

    /// The point `meters` along the line (clamped to its ends).
    func point(atDistance meters: Double) -> LatLon? {
        guard let first = points.first else { return nil }
        guard points.count > 1, meters > 0 else { return first }
        if meters >= length { return points[points.count - 1] }
        // Binary search for the segment.
        var lo = 0, hi = cumulative.count - 1
        while hi - lo > 1 {
            let mid = (lo + hi) / 2
            if cumulative[mid] <= meters { lo = mid } else { hi = mid }
        }
        let span = cumulative[hi] - cumulative[lo]
        let t = span > 0 ? (meters - cumulative[lo]) / span : 0
        let a = points[lo], b = points[hi]
        return LatLon(lat: a.lat + (b.lat - a.lat) * t, lon: a.lon + (b.lon - a.lon) * t)
    }

    /// Parts of the line this much farther than the nearest still count as "as near" (an
    /// out-and-back route walks the same path twice)…
    static let tieToleranceM = 5.0
    /// …and of those, the first one ahead of the hint wins, allowing this much going back (GPS jitter).
    static let backwardSlackM = 20.0

    /// The nearest point of the line to `p`, searching the segments that lie within
    /// `alongRange` metres along the line (all of them when nil). Where the line passes `p` more
    /// than once about as near (within max(5 m, half the distance)), `preferAfter` picks: the first
    /// pass ahead of that distance along (less 20 m), else the last one before it; without it, the
    /// first pass.
    func nearest(to p: LatLon, alongRange: ClosedRange<Double>? = nil, preferAfter hint: Double? = nil) -> RouteMatch? {
        guard let first = points.first else { return nil }
        if points.count == 1 {
            return RouteMatch(distanceAlongM: 0, distanceFromLineM: Geodesy.distance(p, first), segment: 0, point: first)
        }
        // Local metres around `p` (equirectangular): exact enough within tens of kilometres, and
        // only the ranking of segments depends on it; the final distance is a haversine.
        let k = Geodesy.meanRadius * .pi / 180
        let cosLat = cos(p.lat * .pi / 180)
        func local(_ q: LatLon) -> (x: Double, y: Double) {
            var dLon = q.lon - p.lon
            if dLon > 180 { dLon -= 360 } else if dLon < -180 { dLon += 360 }
            return (dLon * k * cosLat, (q.lat - p.lat) * k)
        }
        var lo = 0, hi = points.count - 2
        if let range = alongRange {
            // First segment ending at or after the range's start, last starting at or before its end.
            while lo < hi, cumulative[lo + 1] < range.lowerBound { lo += 1 }
            while hi > lo, cumulative[hi] > range.upperBound { hi -= 1 }
        }
        // Squared distance from `p` (the origin) to segment i, and where along it.
        func segmentDistance(_ i: Int) -> (d2: Double, t: Double) {
            let a = local(points[i]), b = local(points[i + 1])
            let dx = b.x - a.x, dy = b.y - a.y
            let len2 = dx * dx + dy * dy
            var t = 0.0
            if len2 > 0 { t = min(max(-(a.x * dx + a.y * dy) / len2, 0), 1) }
            let x = a.x + t * dx, y = a.y + t * dy
            return (x * x + y * y, t)
        }
        var bestD2 = Double.infinity
        var best = (segment: lo, t: 0.0)
        for i in lo...hi {
            let s = segmentDistance(i)
            if s.d2 < bestD2 {
                bestD2 = s.d2
                best = (segment: i, t: s.t)
            }
        }
        if bestD2.isFinite {
            // Without a hint the first pass wins.
            let hint = hint ?? -Double.infinity
            let bestD = bestD2.squareRoot()
            let limit = bestD + max(Self.tieToleranceM, 0.5 * bestD)
            let limit2 = limit * limit
            var ahead: (segment: Int, t: Double, along: Double)?
            var behind: (segment: Int, t: Double, along: Double)?
            for i in lo...hi {
                let s = segmentDistance(i)
                guard s.d2 <= limit2 else { continue }
                let along = cumulative[i] + (cumulative[i + 1] - cumulative[i]) * s.t
                if along >= hint - Self.backwardSlackM {
                    if along < (ahead?.along ?? .infinity) { ahead = (segment: i, t: s.t, along: along) }
                } else if along > (behind?.along ?? -.infinity) {
                    behind = (segment: i, t: s.t, along: along)
                }
            }
            if let pick = ahead ?? behind { best = (segment: pick.segment, t: pick.t) }
        }
        let s = points[best.segment], e = points[best.segment + 1]
        let q = LatLon(lat: s.lat + (e.lat - s.lat) * best.t, lon: s.lon + (e.lon - s.lon) * best.t)
        let along = cumulative[best.segment] + (cumulative[best.segment + 1] - cumulative[best.segment]) * best.t
        return RouteMatch(distanceAlongM: along, distanceFromLineM: Geodesy.distance(p, q), segment: best.segment, point: q)
    }
}

/// Geometry shared by planning, drawing and following routes.
enum RouteGeometry {
    /// Metres along the polyline at each point.
    static func cumulativeDistances(_ points: [LatLon]) -> [Double] {
        var out = [Double](repeating: 0, count: points.count)
        guard points.count > 1 else { return out }
        var total = 0.0
        for i in 1..<points.count {
            total += Geodesy.distance(points[i - 1], points[i])
            out[i] = total
        }
        return out
    }

    static func length(_ points: [LatLon]) -> Double {
        cumulativeDistances(points).last ?? 0
    }

    /// Douglas–Peucker: the indices of `points` to keep so that no dropped point is farther than
    /// `tolerance` from the kept line (first and last always kept). Iterative, O(n log n) typical.
    static func simplify(_ points: [CGPoint], tolerance: CGFloat) -> [Int] {
        let n = points.count
        guard n > 2 else { return Array(0..<n) }
        var keep = [Bool](repeating: false, count: n)
        keep[0] = true
        keep[n - 1] = true
        let tol2 = tolerance * tolerance
        var stack: [(Int, Int)] = [(0, n - 1)]
        while let (s, e) = stack.popLast() {
            guard e > s + 1 else { continue }
            let a = points[s], b = points[e]
            let dx = b.x - a.x, dy = b.y - a.y
            let len2 = dx * dx + dy * dy
            var worst = -1
            var worstD2: CGFloat = 0
            for i in (s + 1)..<e {
                let p = points[i]
                var d2: CGFloat
                if len2 > 0 {
                    let t = min(max(((p.x - a.x) * dx + (p.y - a.y) * dy) / len2, 0), 1)
                    let x = a.x + t * dx - p.x, y = a.y + t * dy - p.y
                    d2 = x * x + y * y
                } else {
                    let x = p.x - a.x, y = p.y - a.y
                    d2 = x * x + y * y
                }
                if d2 > worstD2 {
                    worstD2 = d2
                    worst = i
                }
            }
            if worst >= 0, worstD2 > tol2 {
                keep[worst] = true
                stack.append((s, worst))
                stack.append((worst, e))
            }
        }
        var out: [Int] = []
        out.reserveCapacity(n / 4 + 2)
        for i in 0..<n where keep[i] { out.append(i) }
        return out
    }

    /// Distance (same units) from `p` to the segment a–b, and where along it (0…1).
    static func distance(from p: CGPoint, toSegment a: CGPoint, _ b: CGPoint) -> (distance: CGFloat, t: CGFloat) {
        let dx = b.x - a.x, dy = b.y - a.y
        let len2 = dx * dx + dy * dy
        var t: CGFloat = 0
        if len2 > 0 { t = min(max(((p.x - a.x) * dx + (p.y - a.y) * dy) / len2, 0), 1) }
        return (hypot(a.x + t * dx - p.x, a.y + t * dy - p.y), t)
    }

    /// Sum of climbs and of descents along elevations, a change counting once it exceeds
    /// `hysteresis` from the last turning level (as `Track.ascent`, both ways).
    static func climbs(_ elevations: [Double], hysteresis: Double = 3) -> (ascent: Double, descent: Double) {
        guard var reference = elevations.first else { return (0, 0) }
        var up = 0.0, down = 0.0
        for e in elevations.dropFirst() where e.isFinite {
            if e - reference >= hysteresis {
                up += e - reference
                reference = e
            } else if reference - e >= hysteresis {
                down += reference - e
                reference = e
            }
        }
        return (up, down)
    }
}
