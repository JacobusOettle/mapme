import SwiftUI

/// Hand-drawn-looking topographic rings around a summit, drawn into a Canvas. Shared by the
/// empty-library hero and the import screen.
enum ContourRingsArt {
    static let contourBrown = Color(red: 0.80, green: 0.58, blue: 0.38)

    /// Draws `rings` closed contours; inner rings are rounder and pulled towards the summit,
    /// which sits up and to the left of `center` like a real asymmetric hill.
    /// Returns the summit position.
    @discardableResult
    static func draw(in gc: inout GraphicsContext, center: CGPoint, radius: CGFloat, time: Double,
                     rings: Int = 8, color: Color = ContourRingsArt.contourBrown, indexColor: Color = ContourRingsArt.contourBrown,
                     lineWidth: CGFloat = 1.2) -> CGPoint {
        let summit = CGPoint(x: center.x - radius * 0.12, y: center.y - radius * 0.10)
        for i in 0..<rings {
            let f = CGFloat(i + 1) / CGFloat(rings)
            let c = CGPoint(x: summit.x + (center.x - summit.x) * f, y: summit.y + (center.y - summit.y) * f)
            let path = ringPath(center: c, radius: radius * f, seed: Double(i), roughness: Double(f), time: time)
            let isIndex = (i + 1) % 4 == 0
            let opacity = 0.30 + 0.55 * Double(1 - f)
            gc.stroke(path,
                      with: .color(isIndex ? indexColor.opacity(min(1, opacity + 0.25)) : color.opacity(opacity)),
                      lineWidth: isIndex ? lineWidth * 1.9 : lineWidth)
        }
        return summit
    }

    static func ringPath(center: CGPoint, radius: CGFloat, seed: Double, roughness: Double, time: Double,
                         points: Int = 120) -> Path {
        var path = Path()
        let amp = 0.35 + 0.65 * roughness
        for k in 0..<points {
            let theta = Double(k) / Double(points) * 2 * Double.pi
            let wobble = 0.10 * sin(3 * theta + seed * 1.3 + time * 0.35)
                + 0.06 * sin(5 * theta - seed * 0.7 - time * 0.22)
                + 0.035 * sin(2 * theta + seed * 2.1 + time * 0.15)
            let r = Double(radius) * (1 + wobble * amp)
            let p = CGPoint(x: center.x + CGFloat(r * cos(theta)), y: center.y + CGFloat(r * sin(theta)))
            if k == 0 {
                path.move(to: p)
            } else {
                path.addLine(to: p)
            }
        }
        path.closeSubpath()
        return path
    }

    /// Red "you are here" dot with an expanding pulse; `phase` runs 0…1.
    static func drawUserDot(in gc: inout GraphicsContext, at p: CGPoint, phase: Double, size: CGFloat = 9) {
        let pr = size + size * 2.6 * CGFloat(phase)
        gc.fill(Path(ellipseIn: CGRect(x: p.x - pr, y: p.y - pr, width: pr * 2, height: pr * 2)),
                with: .color(Theme.userRed.opacity(0.45 * (1 - phase))))
        let halo = size * 2.2
        gc.fill(Path(ellipseIn: CGRect(x: p.x - halo, y: p.y - halo, width: halo * 2, height: halo * 2)),
                with: .radialGradient(Gradient(colors: [Theme.userRed.opacity(0.55), Theme.userRed.opacity(0)]),
                                      center: p, startRadius: 0, endRadius: halo))
        let dot = Path(ellipseIn: CGRect(x: p.x - size, y: p.y - size, width: size * 2, height: size * 2))
        gc.fill(dot, with: .color(Theme.userRed))
        gc.stroke(dot, with: .color(.white), lineWidth: 2.5)
    }
}

/// The empty-library hero: slowly breathing contour rings with a pulsing red dot on the summit.
struct ContourHeroView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { timeline in
            let t = timeline.date.timeIntervalSinceReferenceDate
            Canvas { gc, size in
                let center = CGPoint(x: size.width / 2, y: size.height / 2)
                let radius = min(size.width, size.height) / 2 * 0.9
                gc.fill(Path(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)),
                        with: .radialGradient(Gradient(colors: [Theme.accent.opacity(0.16), Theme.accent.opacity(0)]),
                                              center: center, startRadius: 0, endRadius: radius))
                let summit = ContourRingsArt.draw(in: &gc, center: center, radius: radius, time: t, rings: 9,
                                                  color: ContourRingsArt.contourBrown, indexColor: Theme.accent)
                let phase = t.truncatingRemainder(dividingBy: 2.2) / 2.2
                ContourRingsArt.drawUserDot(in: &gc, at: summit, phase: phase)
            }
        }
        .accessibilityHidden(true)
    }
}
