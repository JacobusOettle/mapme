import CoreGraphics
import Foundation
import Observation

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

/// Apple Maps style user tracking (the location button cycles none → follow → follow with heading → follow).
enum CanvasTrackingMode: String, Equatable, CaseIterable {
    /// The camera stays where you put it.
    case none
    /// The camera follows your position.
    case follow
    /// The camera follows your position and the map turns with your heading.
    case followWithHeading

    var follows: Bool { self != .none }

    /// What the tracking button switches to.
    var next: CanvasTrackingMode {
        switch self {
        case .none: return .follow
        case .follow: return .followWithHeading
        case .followWithHeading: return .follow
        }
    }

    var symbolName: String {
        switch self {
        case .none: return "location"
        case .follow: return "location.fill"
        case .followWithHeading: return "location.north.line.fill"
        }
    }
}

/// Live readouts from the map canvas for SwiftUI chrome (the compass) and commands back to it.
/// The canvas publishes only meaningful changes, so observers re-render rarely.
@MainActor
@Observable
final class MapCanvasController {
    /// Map rotation in degrees (clockwise) relative to the paper's own up. Updated in small steps
    /// while the map turns; read it only from views that should redraw that often.
    private(set) var rotationDegrees: Double = 0
    /// Screen direction of true north at your position (or the view centre), degrees clockwise from up.
    private(set) var northDegrees: Double = 0
    /// The map is turned away from paper-up (the compass shows). Changes only when crossing the threshold.
    private(set) var isRotated = false

    @ObservationIgnored weak var canvas: MapCanvasUIView?

    init() {}

    /// Animate back to paper-up (north-up for a grid-aligned map).
    func resetNorth() {
        canvas?.animateToNorthUp()
    }

    func publish(rotationDegrees r: Double, northDegrees n: Double) {
        if abs(MapAngle.normalizedDegrees(r - rotationDegrees)) > 0.25 || (abs(r) < 0.01 && rotationDegrees != 0) {
            rotationDegrees = abs(r) < 0.01 ? 0 : r
        }
        if abs(MapAngle.normalizedDegrees(n - northDegrees)) > 0.25 {
            northDegrees = n
        }
        let rotated = abs(MapAngle.normalizedDegrees(r)) > 0.75
        if rotated != isRotated { isRotated = rotated }
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

    /// Wraps to -180…180.
    static func normalizedDegrees(_ d: Double) -> Double {
        normalized(radians(d)) * 180 / .pi
    }
}

/// A round-number scale bar ("200 m", "1 km", "500 ft", "2 mi").
struct ScaleBarSpec: Equatable {
    var length: CGFloat
    var label: String
    /// Number at the middle tick, without units ("100" for "200 m").
    var halfLabel: String = ""

    /// - Parameters:
    ///   - metersPerPoint: ground metres per view point at the screen centre.
    ///   - maxLength: longest bar allowed, in points.
    static func make(metersPerPoint: Double, maxLength: CGFloat, units: DistanceUnits) -> ScaleBarSpec? {
        guard metersPerPoint.isFinite, metersPerPoint > 0, maxLength > 0 else { return nil }
        let maxMeters = metersPerPoint * Double(maxLength)
        switch units {
        case .metric:
            let meters = niceNumber(atMost: maxMeters)
            let km = meters >= 1000
            let label = km ? "\(formatted(meters / 1000)) km" : "\(formatted(meters)) m"
            let half = formatted(km ? meters / 2000 : meters / 2)
            return ScaleBarSpec(length: CGFloat(meters / metersPerPoint), label: label, halfLabel: half)
        case .imperial:
            let feet = maxMeters / 0.3048
            if feet < 2640 {
                let f = niceNumber(atMost: feet)
                return ScaleBarSpec(length: CGFloat(f * 0.3048 / metersPerPoint), label: "\(formatted(f)) ft",
                                    halfLabel: formatted(f / 2))
            }
            let miles = niceNumber(atMost: maxMeters / 1609.344)
            return ScaleBarSpec(length: CGFloat(miles * 1609.344 / metersPerPoint), label: "\(formatted(miles)) mi",
                                halfLabel: formatted(miles / 2))
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
