import CoreLocation
import SwiftUI
import UIKit

/// Round frosted map control (46 pt), slightly shrinking while pressed.
struct MapRoundButtonStyle: ButtonStyle {
    var active = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 18, weight: .semibold))
            .foregroundStyle(active ? Theme.accent : Theme.textPrimary)
            .frame(width: 46, height: 46)
            .contentShape(Circle())
            .glass(cornerRadius: 23)
            .shadow(color: .black.opacity(0.35), radius: 8, y: 3)
            .scaleEffect(configuration.isPressed ? 0.9 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

/// Glass capsule button for the bottom bar.
struct MapCapsuleButtonStyle: ButtonStyle {
    var tint: Color = Theme.textPrimary
    var filled: Color? = nil

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(.subheadline, design: .rounded).weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 18)
            .frame(height: 50)
            .background {
                if let filled {
                    Capsule().fill(filled)
                }
            }
            .glass(cornerRadius: 25)
            .shadow(color: .black.opacity(0.35), radius: 10, y: 4)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

enum GeorefLabels {
    static func methodName(_ method: GeorefMethod) -> String {
        switch method {
        case .grid: return "Grid"
        case .labels: return "Labels"
        case .contours: return "Contours"
        case .manual: return "Manual"
        }
    }

    static func chipText(_ g: Georeference?) -> String {
        guard let g else { return "Not located" }
        var text = methodName(g.method)
        if let rms = g.rmsMeters, rms.isFinite {
            text += " ±\(max(1, Int(rms.rounded()))) m"
        }
        return text
    }
}

/// Orientation mode button; in heading-up its arrow keeps pointing at north.
@MainActor
struct OrientationButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let mode = model.settings.orientationMode
        let heading = model.location.headingDegrees ?? 0
        Button {
            withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) {
                model.settings.orientationMode = Self.next(after: mode)
            }
        } label: {
            Image(systemName: mode.symbolName)
                .rotationEffect(.degrees(mode == .headingUp ? -heading : 0))
                .animation(.easeOut(duration: 0.2), value: heading)
                .contentTransition(.symbolEffect(.replace))
        }
        .buttonStyle(MapRoundButtonStyle(active: mode != .northUp))
        .accessibilityLabel("Orientation: \(mode.label)")
        .accessibilityIdentifier("orientationButton")
        .sensoryFeedback(.selection, trigger: mode)
    }

    static func next(after mode: MapOrientationMode) -> MapOrientationMode {
        switch mode {
        case .northUp: return .headingUp
        case .headingUp: return .free
        case .free: return .northUp
        }
    }
}

/// Live position card: coordinates, elevation, accuracy, speed — or where the map is from here.
@MainActor
struct MapInfoCard: View {
    let map: MapDocument
    @Binding var expanded: Bool
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL

    var body: some View {
        let settings = model.settings
        let fix = model.location.location
        let auth = model.location.authorization
        VStack(alignment: .leading, spacing: 6) {
            if auth == .denied || auth == .restricted {
                Label("Location access is off", systemImage: "location.slash.fill")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.warn)
                if expanded {
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                    .font(.caption.weight(.semibold))
                }
            } else if let fix {
                header(fix: fix)
                if expanded {
                    details(fix: fix, settings: settings)
                }
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small).tint(Theme.accent)
                    Text("Waiting for GPS…")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: expanded ? 250 : 170, alignment: .leading)
        .glass(cornerRadius: 14)
        .contentShape(RoundedRectangle(cornerRadius: 14))
        .onTapGesture {
            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { expanded.toggle() }
        }
        .accessibilityIdentifier("infoCard")
    }

    @ViewBuilder
    private func header(fix: CLLocation) -> some View {
        HStack(spacing: 6) {
            Circle()
                .fill(fix.horizontalAccuracy <= 15 ? Theme.good : (fix.horizontalAccuracy <= 50 ? Theme.warn : Theme.bad))
                .frame(width: 7, height: 7)
            Text("±\(Int(max(1, fix.horizontalAccuracy).rounded())) m")
                .font(.system(.subheadline, design: .rounded).weight(.semibold))
                .monospacedDigit()
                .foregroundStyle(Theme.textPrimary)
            if fix.speed > 0.5 {
                Text(GeoFormat.speed(fix.speed, units: model.settings.units))
                    .font(.system(.subheadline, design: .rounded).weight(.medium))
                    .monospacedDigit()
                    .foregroundStyle(Theme.textSecondary)
            }
            Spacer(minLength: 4)
            Image(systemName: expanded ? "chevron.up" : "chevron.down")
                .font(.caption2.weight(.bold))
                .foregroundStyle(Theme.textSecondary)
        }
    }

    @ViewBuilder
    private func details(fix: CLLocation, settings: AppSettings) -> some View {
        let here = LatLon(fix.coordinate)
        Text(GeoFormat.coordinate(here, format: settings.coordinateFormat))
            .font(.system(.footnote, design: .monospaced).weight(.medium))
            .foregroundStyle(Theme.textPrimary)
            .lineLimit(1)
            .minimumScaleFactor(0.7)
            .textSelection(.enabled)
        if fix.verticalAccuracy > 0 {
            Label(GeoFormat.elevation(fix.altitude, units: settings.units), systemImage: "arrow.up.and.down")
                .font(.footnote.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
                .monospacedDigit()
        }
        if let away = Self.offMapText(map: map, here: here, units: settings.units) {
            Label(away, systemImage: "arrow.up.forward.circle.fill")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Theme.warn)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("offMapNotice")
        }
    }

    /// "You are 2.3 km NE of this map" when you are outside the map's footprint.
    static func offMapText(map: MapDocument, here: LatLon, units: DistanceUnits) -> String? {
        guard case .away(let meters, let bearing) = MapProximity.of(map, from: here) else { return nil }
        return "You are \(GeoFormat.distance(meters, units: units)) \(Geodesy.compassPoint(bearing)) of this map"
    }
}

/// Start/stop recording; shows elapsed time and distance while recording.
@MainActor
struct RecordButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let recorder = model.tracks
        let recording = recorder.isRecording
        Button {
            withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) {
                if recording { recorder.stop() } else { recorder.start() }
            }
        } label: {
            HStack(spacing: 10) {
                ZStack {
                    Circle()
                        .fill(Theme.userRed)
                        .frame(width: 16, height: 16)
                    if recording {
                        RoundedRectangle(cornerRadius: 2)
                            .fill(Color.white)
                            .frame(width: 7, height: 7)
                    }
                }
                .frame(width: 22, height: 22)
                if recording {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let elapsed = context.date.timeIntervalSince(recorder.current.startedAt)
                        HStack(spacing: 8) {
                            Text(GeoFormat.duration(max(0, elapsed)))
                                .monospacedDigit()
                            Text(GeoFormat.distance(recorder.current.distance, units: model.settings.units))
                                .monospacedDigit()
                                .foregroundStyle(Theme.textSecondary)
                        }
                    }
                } else {
                    Text("Record")
                }
            }
        }
        .buttonStyle(MapCapsuleButtonStyle(tint: Theme.textPrimary, filled: recording ? Theme.userRed.opacity(0.22) : nil))
        .accessibilityLabel(recording ? "Stop recording" : "Start recording")
        .accessibilityIdentifier("recordButton")
        .sensoryFeedback(.impact(weight: .medium), trigger: recording)
    }
}

/// Details about how the map was located, with "Locate again" and "Calibrate".
@MainActor
struct GeorefInfoView: View {
    let map: MapDocument
    var onLocateAgain: () -> Void
    var onCalibrate: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: MapStatus.symbol(for: map))
                    .foregroundStyle(MapStatus.color(for: map))
                Text(map.georef.map { "Located by \(GeorefLabels.methodName($0.method).lowercased())" } ?? "Not located")
                    .font(.headline)
            }
            if let message = map.georefMessage, !message.isEmpty {
                Text(message)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let g = map.georef {
                HStack(spacing: 16) {
                    stat("Confidence", "\(Int((min(max(g.confidence, 0), 1) * 100).rounded()))%")
                    if let rms = g.rmsMeters, rms.isFinite {
                        stat("Accuracy", "±\(max(1, Int(rms.rounded()))) m")
                    }
                }
            }
            HStack(spacing: 10) {
                Button(action: onLocateAgain) {
                    Label("Locate again", systemImage: "scope")
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .foregroundStyle(Color.black)
                Button(action: onCalibrate) {
                    Label("Calibrate", systemImage: "hand.tap")
                }
                .buttonStyle(.bordered)
            }
            .font(.subheadline.weight(.semibold))
        }
        .padding(16)
        .frame(width: 300, alignment: .leading)
    }

    private func stat(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(Theme.textSecondary)
            Text(value)
                .font(.system(.body, design: .rounded).weight(.semibold))
                .monospacedDigit()
        }
    }
}

/// The selected point of interest: name, distance and direction from you.
@MainActor
struct SelectedPOICard: View {
    let poi: MapPOI
    var onClose: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let units = model.settings.units
        let here = model.location.location.map { LatLon($0.coordinate) }
        HStack(spacing: 12) {
            Image(systemName: poi.kind.symbolName)
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(poi.kind == .waypoint ? Theme.accent : Color(uiColor: Theme.uiPOI))
                .frame(width: 36, height: 36)
                .background(Theme.surfaceRaised, in: Circle())
            VStack(alignment: .leading, spacing: 2) {
                Text(poi.name)
                    .font(.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                HStack(spacing: 8) {
                    if let here {
                        let d = Geodesy.distance(here, poi.position)
                        let b = Geodesy.bearing(here, poi.position)
                        Text("\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(b))")
                            .monospacedDigit()
                    }
                    if let ele = poi.elevationM {
                        Text(GeoFormat.elevation(ele, units: units))
                            .monospacedDigit()
                    }
                }
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
            }
            Spacer(minLength: 8)
            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(Theme.textSecondary)
                    .frame(width: 30, height: 30)
                    .background(Theme.surfaceRaised, in: Circle())
            }
            .accessibilityLabel("Clear selection")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .glass(cornerRadius: 18)
        .shadow(color: .black.opacity(0.35), radius: 10, y: 4)
    }
}

/// Shown when the map has no georeference yet.
@MainActor
struct NotLocatedBanner: View {
    var onCalibrate: () -> Void
    var onRetry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("This map isn't located yet", systemImage: "exclamationmark.triangle.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.warn)
            Text("Calibrate it by matching a few points, or try locating it again near where it was printed.")
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Button(action: onCalibrate) {
                    Label("Calibrate", systemImage: "hand.tap.fill")
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .foregroundStyle(Color.black)
                .accessibilityIdentifier("bannerCalibrateButton")
                Button(action: onRetry) {
                    Label("Try again", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("bannerRetryButton")
            }
            .font(.subheadline.weight(.semibold))
        }
        .padding(14)
        .frame(maxWidth: 420, alignment: .leading)
        .glass(cornerRadius: 18)
    }
}
