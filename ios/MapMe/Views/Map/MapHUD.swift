import CoreLocation
import SwiftUI
import UIKit

/// Round floating map control (44 pt): Liquid Glass on iOS 26, frosted material before.
struct MapRoundButtonStyle: ButtonStyle {
    var active = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 17, weight: .semibold))
            .foregroundStyle(active ? Color.blue : Color.primary)
            .frame(width: 44, height: 44)
            .contentShape(Circle())
            .floatingChrome(Circle(), interactive: true)
            .scaleEffect(configuration.isPressed ? 0.92 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

/// Glass capsule button.
struct MapCapsuleButtonStyle: ButtonStyle {
    var tint: Color = .primary
    var filled: Color? = nil

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(.subheadline, design: .rounded).weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 18)
            .frame(height: 44)
            .background {
                if let filled {
                    Capsule().fill(filled)
                }
            }
            .floatingChrome(Capsule(), interactive: true)
            .scaleEffect(configuration.isPressed ? 0.95 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

/// A plain button that shrinks a little while pressed.
struct PressScaleButtonStyle: ButtonStyle {
    var scale: CGFloat = 0.92

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? scale : 1)
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

    /// Status line under the map name: "Located · grid ±1 m", "Not located".
    static func statusLine(_ g: Georeference?) -> String {
        guard let g else { return "Not located · tap to fix" }
        return "Located · " + chipText(g).lowercased()
    }
}

// MARK: - Top bar

/// Map name plus a small status line with a coloured dot; tapping shows how the map was located.
@MainActor
struct MapTitleCapsule: View {
    let map: MapDocument
    @Binding var showInfo: Bool
    var onLocateAgain: () -> Void
    var onCalibrate: () -> Void

    var body: some View {
        Button {
            showInfo = true
        } label: {
            VStack(alignment: .leading, spacing: 1) {
                Text(map.name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                HStack(spacing: 5) {
                    Circle()
                        .fill(MapStatus.color(for: map))
                        .frame(width: 6, height: 6)
                    Text(GeorefLabels.statusLine(map.georef))
                        .font(.caption2.weight(.medium))
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .padding(.leading, 16)
            .padding(.trailing, 18)
            .frame(minHeight: 44)
            .contentShape(Capsule())
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.96))
        .floatingChrome(Capsule(), interactive: true)
        .frame(maxWidth: 260, alignment: .leading)
        .accessibilityLabel("\(map.name), \(GeorefLabels.statusLine(map.georef))")
        .accessibilityIdentifier("georefChip")
        .popover(isPresented: $showInfo, arrowEdge: .top) {
            GeorefInfoView(map: map, onLocateAgain: onLocateAgain, onCalibrate: onCalibrate)
                .presentationCompactAdaptation(.popover)
        }
    }
}

/// Apple Maps control stack: one glass group with the Layers menu and the tracking button.
@MainActor
struct CanvasControlStack: View {
    let map: MapDocument
    @Binding var tracking: CanvasTrackingMode

    var body: some View {
        VStack(spacing: 0) {
            LayersMenu()
            Rectangle()
                .fill(Color.primary.opacity(0.16))
                .frame(width: 28, height: 0.5)
            TrackingModeButton(map: map, tracking: $tracking)
        }
        .frame(width: 44)
        .floatingChrome(RoundedRectangle(cornerRadius: 22, style: .continuous))
        .sensoryFeedback(.selection, trigger: tracking)
    }
}

/// Map style, trail and map-label toggles, clear trail.
@MainActor
private struct LayersMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var settings = model.settings
        Menu {
            Picker(selection: $settings.mapAppearance) {
                ForEach(MapAppearance.allCases) { appearance in
                    Label(appearance.label, systemImage: appearance.symbolName).tag(appearance)
                }
            } label: {
                Text("Map Appearance")
            }
            .pickerStyle(.inline)
            Section {
                Toggle(isOn: $settings.showTrack) {
                    Label("Trail", systemImage: "figure.walk")
                }
                Toggle(isOn: $settings.showMapLabels) {
                    Label("Map Labels as Markers", systemImage: "mappin.and.ellipse")
                }
            }
            Section {
                Button(role: .destructive) {
                    model.tracks.clearCurrent()
                } label: {
                    Label("Clear Trail", systemImage: "trash")
                }
            }
        } label: {
            Image(systemName: "square.2.layers.3d")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .accessibilityLabel("Map layers")
        .accessibilityIdentifier("layersButton")
    }
}

/// Apple Maps location button: not following → follow → follow with heading → follow.
@MainActor
struct TrackingModeButton: View {
    let map: MapDocument
    @Binding var tracking: CanvasTrackingMode
    @Environment(AppModel.self) private var model

    var body: some View {
        let canFollow = Self.userIsNearMap(map, fix: model.location.location)
        Button {
            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                let next = tracking.next
                tracking = next
                // Remember heading mode for the next map, like the old orientation setting.
                if next == .followWithHeading {
                    model.settings.orientationMode = .headingUp
                } else if next == .follow, model.settings.orientationMode == .headingUp {
                    model.settings.orientationMode = .northUp
                }
            }
        } label: {
            Image(systemName: tracking.symbolName)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(tracking.follows ? Color.blue : Color.primary)
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.88))
        .opacity(canFollow || tracking.follows ? 1 : 0.55)
        .accessibilityLabel(Self.trackingLabel(tracking))
        .accessibilityValue(Self.trackingValue(tracking))
        .accessibilityIdentifier("trackingButton")
    }

    /// You are on (or within a kilometre of) the map, so following can show you.
    static func userIsNearMap(_ map: MapDocument, fix: CLLocation?) -> Bool {
        guard let fix else { return false }
        let proximity = MapProximity.of(map, from: LatLon(fix.coordinate))
        if proximity.isOnMap { return true }
        if case .away(let meters, _) = proximity { return meters < 1000 }
        return false
    }

    static func trackingLabel(_ mode: CanvasTrackingMode) -> String {
        switch mode {
        case .none: return "Follow my position"
        case .follow: return "Following your position. Tap to follow your heading"
        case .followWithHeading: return "Following your heading. Tap for north up"
        }
    }

    /// Stable value for UI tests: "none", "follow" or "heading".
    static func trackingValue(_ mode: CanvasTrackingMode) -> String {
        switch mode {
        case .none: return "none"
        case .follow: return "follow"
        case .followWithHeading: return "heading"
        }
    }
}

/// Apple-style compass: N and a red needle, turning with the map; shown only while the map is
/// rotated. Tap to turn back to north up.
@MainActor
struct CanvasCompassButton: View {
    let controller: MapCanvasController

    var body: some View {
        let north = controller.northDegrees
        Button {
            controller.resetNorth()
        } label: {
            CompassFace()
                .rotationEffect(.degrees(north))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.9))
        .floatingChrome(Circle(), interactive: true)
        .accessibilityLabel("Compass, \(Int(MapAngle.normalizedDegrees(-north).rounded()))°. Tap to turn north up")
        .accessibilityIdentifier("compassButton")
    }
}

/// The compass dial, north up (the caller rotates it).
private struct CompassFace: View {
    var body: some View {
        ZStack {
            Canvas { context, size in
                let c = CGPoint(x: size.width / 2, y: size.height / 2)
                let r = min(size.width, size.height) / 2
                for i in 0..<24 where i % 6 != 0 {
                    let a = Double(i) / 24 * 2 * .pi
                    let major = i % 3 == 0
                    var tick = Path()
                    let outer = r - 4
                    let inner = outer - (major ? 4 : 2.5)
                    tick.move(to: CGPoint(x: c.x + CGFloat(sin(a)) * inner, y: c.y - CGFloat(cos(a)) * inner))
                    tick.addLine(to: CGPoint(x: c.x + CGFloat(sin(a)) * outer, y: c.y - CGFloat(cos(a)) * outer))
                    context.stroke(tick, with: .color(Color.primary.opacity(major ? 0.55 : 0.3)), lineWidth: 1)
                }
                var needle = Path()
                needle.move(to: CGPoint(x: c.x, y: c.y - r + 2))
                needle.addLine(to: CGPoint(x: c.x + 4.5, y: c.y - r + 10))
                needle.addLine(to: CGPoint(x: c.x - 4.5, y: c.y - r + 10))
                needle.closeSubpath()
                context.fill(needle, with: .color(Color(red: 1.0, green: 0.23, blue: 0.19)))
            }
            Text("N")
                .font(.system(size: 13, weight: .bold, design: .rounded))
                .foregroundStyle(.primary)
                .offset(y: 2)
        }
    }
}

// MARK: - Record

/// Big round record button: white ring with a red disc that morphs into a pulsing rounded square
/// while recording.
@MainActor
struct RecordButton: View {
    var size: CGFloat = 60
    @Environment(AppModel.self) private var model

    var body: some View {
        let recorder = model.tracks
        let recording = recorder.isRecording
        // Kept out of the view expression so the type checker has less to solve at once.
        let discSide: CGFloat = recording ? size * 0.42 : size * 0.8
        let discCorner: CGFloat = recording ? 7 : size * 0.4
        Button {
            withAnimation(.spring(response: 0.4, dampingFraction: 0.75)) {
                if recording { recorder.stop() } else { recorder.start() }
            }
        } label: {
            ZStack {
                Circle()
                    .strokeBorder(Color.white, lineWidth: 4)
                    .shadow(color: Color.black.opacity(0.2), radius: 2, y: 1)
                RoundedRectangle(cornerRadius: discCorner, style: .continuous)
                    .fill(Theme.record.gradient)
                    .frame(width: discSide, height: discSide)
                    .phaseAnimator([false, true]) { content, phase in
                        content
                            .opacity(recording && phase ? 0.6 : 1)
                            .scaleEffect(recording && phase ? 0.9 : 1)
                    } animation: { _ in
                        .easeInOut(duration: 0.85)
                    }
            }
            .frame(width: size, height: size)
            .contentShape(Circle())
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.9))
        .accessibilityLabel(recording ? "Stop recording" : "Start recording")
        .accessibilityIdentifier("recordButton")
        .sensoryFeedback(.impact(weight: .medium), trigger: recording)
    }
}

// MARK: - Cards

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
                    .foregroundStyle(.secondary)
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
                .foregroundStyle(.secondary)
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
            POIGlyphMarker(kind: poi.kind, size: 40)
            VStack(alignment: .leading, spacing: 2) {
                Text(poi.name.isEmpty ? POIKindStyle.name(for: poi.kind) : poi.name)
                    .font(.headline)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    if let here {
                        let d = Geodesy.distance(here, poi.position)
                        let b = Geodesy.bearing(here, poi.position)
                        Text("\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(b))")
                            .monospacedDigit()
                    }
                    if let ele = poi.elevationM {
                        Text("·")
                        Text(GeoFormat.elevation(ele, units: units))
                            .monospacedDigit()
                    }
                    Text("·")
                    Text(POIKindStyle.sourceName(for: poi.source))
                        .lineLimit(1)
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .bold))
                    .foregroundStyle(.secondary)
                    .frame(width: 30, height: 30)
                    .background(Color.primary.opacity(0.1), in: Circle())
            }
            .buttonStyle(PressScaleButtonStyle())
            .accessibilityLabel("Clear selection")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
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
                .foregroundStyle(.secondary)
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
        .frame(maxWidth: 520, alignment: .leading)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
    }
}
