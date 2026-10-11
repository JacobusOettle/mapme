import SwiftUI

/// Between the two panes: what to do next, the numbered pairs with their residuals,
/// overall fit quality, undo and clear.
struct CalibrationGuidanceBar: View {
    let step: CalibrationStep
    let pairs: [CalibrationPair]
    let fit: CalibrationFit?
    let fitError: String?
    /// nil when there is no fit or no GPS fix.
    let userOnMap: Bool?
    let canUndo: Bool
    var onUndo: () -> Void
    var onClear: () -> Void
    var onDelete: (Int) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: step.symbol)
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(step.symbol.hasPrefix("exclamationmark") ? Theme.warn : Theme.accent)
                    .frame(width: 26)
                    .contentTransition(.symbolEffect(.replace))
                Text(step.text)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .id(step.text)
                    .transition(.opacity)
                    .accessibilityIdentifier("calibrationStep")
                HStack(spacing: 6) {
                    iconButton("arrow.uturn.backward", label: "Undo", enabled: canUndo, action: onUndo)
                    iconButton("trash", label: "Clear all points", enabled: !pairs.isEmpty, action: onClear)
                }
            }

            if !pairs.isEmpty || fitError != nil {
                HStack(spacing: 8) {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(Array(pairs.enumerated()), id: \.element.id) { index, pair in
                                pairChip(index: index, pair: pair)
                            }
                            if let userOnMap {
                                TintedChip(text: userOnMap ? "You: on map" : "You: off map",
                                           symbol: "location.fill", tint: Theme.userRed)
                            }
                        }
                        .padding(.vertical, 1)
                    }
                    fitBadge
                }
            }

            if let fitError {
                Label(fitError, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(Theme.warn)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(12)
        .glass()
        .animation(.snappy, value: step)
        .animation(.snappy, value: pairs.count)
    }

    /// Residuals only mean something once there are more points than the model needs.
    private func residual(_ index: Int) -> Double? {
        guard let fit, fit.isChecked, index < fit.residuals.count, fit.residuals[index].isFinite else { return nil }
        return fit.residuals[index]
    }

    private func pairChip(index: Int, pair: CalibrationPair) -> some View {
        let r = residual(index)
        let tint = r.map { CalibrationFit.residualTint($0) }
        var a11y = "Point \(index + 1)"
        if let r {
            a11y += ", off by " + Self.meters(r)
        }
        return Menu {
            Button(role: .destructive) {
                onDelete(index)
            } label: {
                Label("Delete point \(index + 1)", systemImage: "trash")
            }
        } label: {
            HStack(spacing: 5) {
                Text("\(index + 1)")
                    .font(.system(size: 11, weight: .heavy, design: .rounded))
                    .foregroundStyle(Color.black)
                    .frame(width: 18, height: 18)
                    .background(CalibrationPalette.color(index), in: Circle())
                if pair.fromGPS {
                    Image(systemName: "location.fill")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Theme.userRed)
                }
                if let r {
                    Text(Self.meters(r))
                        .font(.caption.weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(tint ?? Theme.textPrimary)
                }
            }
            .padding(.leading, 4)
            .padding(.trailing, r == nil && !pair.fromGPS ? 4 : 9)
            .padding(.vertical, 4)
            .background(Theme.surfaceRaised, in: Capsule())
            .overlay(Capsule().stroke((tint ?? Color.white).opacity(0.3), lineWidth: 1))
        }
        .accessibilityLabel(a11y)
    }

    @ViewBuilder
    private var fitBadge: some View {
        if let fit {
            VStack(alignment: .trailing, spacing: 1) {
                if fit.isChecked {
                    Text("\u{00B1}" + Self.meters(fit.rms))
                        .font(.subheadline.weight(.bold))
                        .monospacedDigit()
                        .foregroundStyle(CalibrationFit.residualTint(fit.rms))
                } else {
                    Text("Rough fit")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.warn)
                }
                Text(fit.model.isEmpty ? "fit" : fit.model)
                    .font(.caption2)
                    .foregroundStyle(Theme.textSecondary)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("calibrationFit")
        } else if pairs.count == 1 {
            Text("1 more point")
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
        }
    }

    private func iconButton(_ symbol: String, label: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.textPrimary)
                .frame(width: 34, height: 34)
                .background(Theme.surfaceRaised, in: Circle())
                .overlay(Circle().stroke(Theme.stroke, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.35)
        .accessibilityLabel(label)
    }

    static func meters(_ m: Double) -> String {
        m < 1000 ? "\(Int(m.rounded())) m" : String(format: "%.1f km", m / 1000)
    }
}

/// Contour-colour picker controls (replaces the guidance bar while picking).
struct CalibrationColourPanel: View {
    /// The colour that would be used when locating (nil = built-in colours).
    let rgb: [Int]?
    /// Picked during this session (vs. saved earlier).
    let isPicked: Bool
    let sampling: Bool
    var onLocate: () -> Void
    var onAutomatic: () -> Void
    var onDone: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                swatch
                VStack(alignment: .leading, spacing: 3) {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                        .monospacedDigit()
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                if sampling {
                    ProgressView()
                }
            }

            HStack(spacing: 8) {
                Button(action: onLocate) {
                    Label(locateTitle, systemImage: "scope")
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 10)
                        .foregroundStyle(Color.black)
                        .background(Theme.accent, in: Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("locateWithColour")

                if rgb != nil {
                    Button(action: onAutomatic) {
                        Text("Auto")
                            .font(.subheadline.weight(.semibold))
                            .padding(.horizontal, 14)
                            .padding(.vertical, 10)
                            .foregroundStyle(Theme.textPrimary)
                            .background(Theme.surfaceRaised, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Use automatic contour colours")
                }

                Button(action: onDone) {
                    Text("Done")
                        .font(.subheadline.weight(.semibold))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .foregroundStyle(Theme.textPrimary)
                        .background(Theme.surfaceRaised, in: Capsule())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(12)
        .glass()
        .animation(.snappy, value: rgb)
    }

    private var locateTitle: String {
        rgb == nil ? "Locate again" : "Locate again with this colour"
    }

    private var title: String {
        guard rgb != nil else { return "Automatic contour colours" }
        return isPicked ? "Picked contour colour" : "Saved contour colour"
    }

    private var subtitle: String {
        guard let rgb, rgb.count >= 3 else {
            return "Tap a contour line on your map to use its colour. Zoom in so your finger lands on the line."
        }
        return "RGB \(rgb[0]), \(rgb[1]), \(rgb[2]) \u{00B7} tap another line to pick again"
    }

    @ViewBuilder
    private var swatch: some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        if let rgb {
            shape
                .fill(CalibrationPalette.swatch(rgb))
                .frame(width: 44, height: 44)
                .overlay(shape.stroke(Color.white.opacity(0.5), lineWidth: 1.5))
        } else {
            shape
                .fill(AngularGradient(colors: [.brown, .orange, Color(red: 0.85, green: 0.7, blue: 0.5), .brown],
                                      center: .center))
                .frame(width: 44, height: 44)
                .overlay(Image(systemName: "eyedropper").font(.system(size: 16, weight: .bold)).foregroundStyle(Color.white))
                .overlay(shape.stroke(Theme.stroke, lineWidth: 1))
        }
    }
}
