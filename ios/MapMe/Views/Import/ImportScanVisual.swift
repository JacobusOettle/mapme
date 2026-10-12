import SwiftUI

/// Radar sweep over contour rings while a map is being imported and located; settles into a
/// glowing red "you are here" dot when it worked.
struct ImportScanVisual: View {
    var phase: ImportJob.Phase
    /// A wide search: the share of its circle searched so far (0…1), drawn as a disc growing out
    /// from the centre; nil for a normal import.
    var searchedFraction: Double? = nil

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let phase = self.phase
        let searched = searchedFraction
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { timeline in
            let t = timeline.date.timeIntervalSinceReferenceDate
            Canvas { gc, size in
                ImportScanArt.draw(in: &gc, size: size, time: t, phase: phase, searchedFraction: searched)
            }
        }
        .accessibilityHidden(true)
    }
}

enum ImportScanArt {
    static func draw(in gc: inout GraphicsContext, size: CGSize, time t: Double, phase: ImportJob.Phase,
                     searchedFraction: Double? = nil) {
        let isWorking = phase == .importing || phase == .locating
        let center = CGPoint(x: size.width / 2, y: size.height / 2)
        let radius = min(size.width, size.height) / 2 - 2
        let disc = CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)
        let tint: Color
        switch phase {
        case .importing, .locating, .succeeded: tint = Theme.accent
        case .needsCalibration: tint = Theme.warn
        case .failed: tint = Theme.bad
        }

        gc.fill(Path(ellipseIn: disc),
                with: .radialGradient(Gradient(colors: [tint.opacity(0.14), tint.opacity(0.02)]),
                                      center: center, startRadius: 0, endRadius: radius))

        var rings = gc
        rings.clip(to: Path(ellipseIn: disc))
        let ringColor = phase == .succeeded ? Theme.accent : ContourRingsArt.contourBrown
        let summit = ContourRingsArt.draw(in: &rings, center: center, radius: radius * 0.92, time: t * (isWorking ? 1.6 : 0.6),
                                          rings: 8, color: ringColor, indexColor: tint)

        // Range rings and crosshair.
        for f in [0.33, 0.66, 1.0] {
            let r = radius * CGFloat(f)
            gc.stroke(Path(ellipseIn: CGRect(x: center.x - r, y: center.y - r, width: r * 2, height: r * 2)),
                      with: .color(tint.opacity(f == 1.0 ? 0.45 : 0.14)), lineWidth: f == 1.0 ? 1.5 : 1)
        }
        var cross = Path()
        cross.move(to: CGPoint(x: center.x - radius, y: center.y))
        cross.addLine(to: CGPoint(x: center.x + radius, y: center.y))
        cross.move(to: CGPoint(x: center.x, y: center.y - radius))
        cross.addLine(to: CGPoint(x: center.x, y: center.y + radius))
        gc.stroke(cross, with: .color(tint.opacity(0.12)), lineWidth: 1)

        // Wide search: the area searched so far (its share of the circle's area), with a softly
        // breathing front where the search is now.
        if isWorking, let searchedFraction {
            let r = radius * CGFloat(min(max(searchedFraction, 0), 1).squareRoot())
            if r > 1 {
                let searched = CGRect(x: center.x - r, y: center.y - r, width: r * 2, height: r * 2)
                gc.fill(Path(ellipseIn: searched),
                        with: .radialGradient(Gradient(colors: [tint.opacity(0.05), tint.opacity(0.20)]),
                                              center: center, startRadius: 0, endRadius: r))
                let breathe = 0.55 + 0.35 * sin(t * 3)
                gc.stroke(Path(ellipseIn: searched), with: .color(tint.opacity(breathe)), lineWidth: 2)
            }
        }

        if isWorking {
            let angle = t * 2.4
            var sweep = gc
            sweep.clip(to: Path(ellipseIn: disc))
            sweep.fill(Path(ellipseIn: disc),
                       with: .conicGradient(Gradient(stops: [
                           Gradient.Stop(color: tint.opacity(0), location: 0),
                           Gradient.Stop(color: tint.opacity(0), location: 0.72),
                           Gradient.Stop(color: tint.opacity(0.38), location: 1),
                       ]), center: center, angle: .radians(angle)))
            var beam = Path()
            beam.move(to: center)
            beam.addLine(to: CGPoint(x: center.x + radius * CGFloat(cos(angle)), y: center.y + radius * CGFloat(sin(angle))))
            gc.stroke(beam, with: .color(tint.opacity(0.9)), lineWidth: 1.5)

            // Blips that light up as the beam passes over them.
            for k in 0..<5 {
                let a = Double(k) * 1.37 + 0.4
                let d = radius * CGFloat(0.25 + 0.14 * Double(k))
                let since = (angle - a).truncatingRemainder(dividingBy: 2 * Double.pi)
                let age = since < 0 ? since + 2 * Double.pi : since
                let alpha = max(0, 1 - age / 2.2)
                guard alpha > 0 else { continue }
                let p = CGPoint(x: center.x + d * CGFloat(cos(a)), y: center.y + d * CGFloat(sin(a)))
                gc.fill(Path(ellipseIn: CGRect(x: p.x - 3, y: p.y - 3, width: 6, height: 6)),
                        with: .color(tint.opacity(0.85 * alpha)))
            }
        } else if phase == .succeeded {
            let pulse = t.truncatingRemainder(dividingBy: 2.0) / 2.0
            ContourRingsArt.drawUserDot(in: &gc, at: summit, phase: pulse, size: 11)
        }
    }
}

/// Thin glowing progress bar.
struct ImportProgressBar: View {
    var fraction: Double

    var body: some View {
        GeometryReader { geo in
            let f = min(max(fraction, 0), 1)
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.08))
                Capsule()
                    .fill(LinearGradient(colors: [Theme.accent.opacity(0.55), Theme.accent],
                                         startPoint: .leading, endPoint: .trailing))
                    .frame(width: max(6, geo.size.width * CGFloat(f)))
                    .shadow(color: Theme.accent.opacity(0.7), radius: 6)
            }
        }
        .frame(height: 6)
        .animation(.easeInOut(duration: 0.35), value: fraction)
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue("\(Int((min(max(fraction, 0), 1) * 100).rounded())) percent")
    }
}

/// Full-width capsule buttons for the import screen.
struct ImportActionButtonStyle: ButtonStyle {
    enum Kind { case primary, secondary, plain }
    var kind: Kind = .primary

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 15)
            .foregroundStyle(foreground)
            .background { capsuleBackground }
            .overlay(
                Capsule().stroke(kind == .secondary ? Theme.stroke : Color.clear, lineWidth: 1)
            )
            .contentShape(Capsule())
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }

    private var foreground: Color {
        switch kind {
        case .primary: return .black
        case .secondary: return Theme.textPrimary
        case .plain: return Theme.accent
        }
    }

    @ViewBuilder
    private var capsuleBackground: some View {
        switch kind {
        case .primary:
            Capsule().fill(Theme.accent).shadow(color: Theme.accent.opacity(0.4), radius: 14, y: 4)
        case .secondary:
            Capsule().fill(.ultraThinMaterial)
        case .plain:
            Color.clear
        }
    }
}
