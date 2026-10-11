import CoreGraphics
import Foundation

/// What part of the map the canvas shows. The map pixel `center` sits at the middle of the
/// view, `zoom` is view points per map pixel and `rotation` turns the map clockwise (radians).
struct CanvasCamera: Equatable {
    var center: CGPoint
    var zoom: CGFloat
    var rotation: CGFloat

    /// Map pixel → view point.
    func transform(viewSize: CGSize) -> CGAffineTransform {
        CGAffineTransform(translationX: viewSize.width / 2, y: viewSize.height / 2)
            .rotated(by: rotation)
            .scaledBy(x: zoom, y: zoom)
            .translatedBy(x: -center.x, y: -center.y)
    }

    func viewPoint(forMap p: CGPoint, viewSize: CGSize) -> CGPoint {
        let dx = (p.x - center.x) * zoom
        let dy = (p.y - center.y) * zoom
        let c = cos(rotation), s = sin(rotation)
        return CGPoint(x: viewSize.width / 2 + dx * c - dy * s, y: viewSize.height / 2 + dx * s + dy * c)
    }

    func mapPoint(forView v: CGPoint, viewSize: CGSize) -> CGPoint {
        let r = unrotated(v, viewSize: viewSize)
        return CGPoint(x: center.x + r.x / zoom, y: center.y + r.y / zoom)
    }

    /// The camera centre that puts map pixel `p` at view point `v` (same zoom and rotation).
    func center(placing p: CGPoint, at v: CGPoint, viewSize: CGSize) -> CGPoint {
        let r = unrotated(v, viewSize: viewSize)
        return CGPoint(x: p.x - r.x / zoom, y: p.y - r.y / zoom)
    }

    /// View offset from the middle, rotated back into map axes (still in points).
    private func unrotated(_ v: CGPoint, viewSize: CGSize) -> CGPoint {
        let dx = v.x - viewSize.width / 2
        let dy = v.y - viewSize.height / 2
        let c = cos(rotation), s = sin(rotation)
        return CGPoint(x: dx * c + dy * s, y: -dx * s + dy * c)
    }
}

enum MapAngle {
    /// Wraps to -π…π.
    static func normalized(_ a: Double) -> Double {
        var r = a.truncatingRemainder(dividingBy: 2 * .pi)
        if r > .pi { r -= 2 * .pi }
        if r < -.pi { r += 2 * .pi }
        return r
    }

    static func radians(_ degrees: Double) -> Double { degrees * .pi / 180 }
}

/// A round-number scale bar ("200 m", "1 km", "500 ft", "2 mi").
struct ScaleBarSpec: Equatable {
    var length: CGFloat
    var label: String

    /// - Parameters:
    ///   - metersPerPoint: ground metres per view point at the screen centre.
    ///   - maxLength: longest bar allowed, in points.
    static func make(metersPerPoint: Double, maxLength: CGFloat, units: DistanceUnits) -> ScaleBarSpec? {
        guard metersPerPoint.isFinite, metersPerPoint > 0, maxLength > 0 else { return nil }
        let maxMeters = metersPerPoint * Double(maxLength)
        switch units {
        case .metric:
            let meters = niceNumber(atMost: maxMeters)
            let label = meters >= 1000 ? "\(formatted(meters / 1000)) km" : "\(formatted(meters)) m"
            return ScaleBarSpec(length: CGFloat(meters / metersPerPoint), label: label)
        case .imperial:
            let feet = maxMeters / 0.3048
            if feet < 2640 {
                let f = niceNumber(atMost: feet)
                return ScaleBarSpec(length: CGFloat(f * 0.3048 / metersPerPoint), label: "\(formatted(f)) ft")
            }
            let miles = niceNumber(atMost: maxMeters / 1609.344)
            return ScaleBarSpec(length: CGFloat(miles * 1609.344 / metersPerPoint), label: "\(formatted(miles)) mi")
        }
    }

    /// Largest 1, 2 or 5 × 10ⁿ not above `value`.
    static func niceNumber(atMost value: Double) -> Double {
        guard value > 0 else { return 0 }
        let exponent = floor(log10(value))
        let base = pow(10, exponent)
        for step in [5.0, 2.0, 1.0] where step * base <= value * (1 + 1e-9) {
            return step * base
        }
        return base
    }

    private static func formatted(_ v: Double) -> String {
        if v >= 1, v == v.rounded() { return String(Int(v)) }
        return String(format: "%g", v)
    }
}
