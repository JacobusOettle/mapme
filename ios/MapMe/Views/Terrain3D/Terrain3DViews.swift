import CoreLocation
import SwiftUI

// The 3D view (docs/design/explorer.md, 3D view): the side column's button and the full-screen
// view of the map draped on the terrain around it.

/// Side column: the 3D button (a cube), presenting `Terrain3DScreen` full screen over the map.
/// Dimmed when the map is not located (a tap says why).
@MainActor
struct Terrain3DButton: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        let located = map.georef != nil
        Button {
            open()
        } label: {
            Image(systemName: "cube")
        }
        .buttonStyle(MapRoundButtonStyle())
        .opacity(located ? 1 : 0.55)
        .accessibilityLabel("3D view")
        .accessibilityHint(located ? "Shows the map draped on the terrain around it" : "Locate this map first")
        .accessibilityIdentifier("terrain3DButton")
        .task(id: map.id) {
            // `-open3D` (Debug): once the sample map shows.
            guard located, Terrain3DFeature.takeOpenAtLaunch() else { return }
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            if !Task.isCancelled { open() }
        }
    }

    private func open() {
        guard map.georef != nil else {
            context.show(MapToast(text: "Locate this map to see it in 3D", symbol: "cube"))
            return
        }
        let id = map.id
        context.cover = MapPresentation {
            AnyView(Terrain3DScreen(mapID: id))
        }
    }
}

/// The map draped on the terrain in 3D, with the surrounding area (the continuation margin),
/// POIs, waypoints, notes, routes, the track and you. Turn, tilt, move and zoom it; the compass
/// faces it north, Reset goes back to the start, the height menu sets the exaggeration.
@MainActor
struct Terrain3DScreen: View {
    let mapID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var session = Terrain3DSession()
    @State private var controller = Terrain3DController()
    @State private var resets = 0

    var body: some View {
        let map = model.library.map(id: mapID)
        ZStack {
            Theme.background.ignoresSafeArea()
            Terrain3DLiveScene(session: session, controller: controller, mapName: map?.name ?? "Map")
                .ignoresSafeArea()
            chrome(map: map)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("terrain3DView")
        .task(id: buildKey) {
            await session.build(mapID: mapID, app: model)
        }
        .onChange(of: model.geoItems.revision) { _, _ in refreshItems() }
        .onChange(of: model.settings.showPOIs) { _, _ in refreshItems() }
        .onChange(of: model.settings.showOtherMapsPOIs) { _, _ in refreshItems() }
        .onChange(of: model.library.maps) { _, _ in refreshItems() }
        .sensoryFeedback(.selection, trigger: model.settings.terrainExaggeration)
        .sensoryFeedback(.selection, trigger: resets)
        .sensoryFeedback(.success, trigger: session.builds)
    }

    /// Builds again for another appearance (dark or paper drape) and on Retry.
    private var buildKey: String {
        "\(mapID.uuidString)-\(model.settings.mapAppearance.rawValue)-\(session.attempt)"
    }

    private func refreshItems() {
        session.refreshItems(mapID: mapID, app: model)
    }

    // MARK: - Chrome

    private func chrome(map: MapDocument?) -> some View {
        VStack(spacing: 0) {
            HStack(alignment: .top, spacing: 10) {
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(MapRoundButtonStyle())
                .accessibilityLabel("Close 3D view")
                .accessibilityIdentifier("terrain3DClose")

                titleCapsule(map: map)

                Spacer(minLength: 0)

                VStack(spacing: 10) {
                    Terrain3DCompassButton(controller: controller)
                    controlStack
                }
            }
            .padding(.horizontal, 12)
            .padding(.top, 4)

            Spacer(minLength: 0)

            status
                .padding(.horizontal, 12)
                .padding(.bottom, 12)
        }
    }

    private func titleCapsule(map: MapDocument?) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(map?.name ?? "Map")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.primary)
                .lineLimit(1)
            Text("3D · height \(Self.exaggerationLabel(model.settings.terrainExaggeration))")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 44)
        .floatingChrome(Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("terrain3DTitle")
    }

    /// Reset and the height menu, one glass group (as the map's layers and tracking).
    private var controlStack: some View {
        @Bindable var settings = model.settings
        return VStack(spacing: 0) {
            Button {
                resets += 1
                controller.resetCamera()
            } label: {
                Image(systemName: "scope")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.primary)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(PressScaleButtonStyle())
            .accessibilityLabel("Reset view")
            .accessibilityIdentifier("terrain3DReset")

            Rectangle()
                .fill(Color.primary.opacity(0.16))
                .frame(width: 28, height: 0.5)

            Menu {
                Picker(selection: $settings.terrainExaggeration) {
                    ForEach(Terrain3DLimits.exaggerationChoices, id: \.self) { value in
                        Text(Self.exaggerationTitle(value)).tag(value)
                    }
                } label: {
                    Text("Height")
                }
                .pickerStyle(.inline)
            } label: {
                Text(Self.exaggerationLabel(settings.terrainExaggeration))
                    .font(.system(size: 14, weight: .bold, design: .rounded))
                    .foregroundStyle(.primary)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("Height, \(Self.exaggerationTitle(settings.terrainExaggeration))")
            .accessibilityIdentifier("terrain3DExaggeration")
        }
        .frame(width: 44)
        .floatingChrome(RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    /// "1.5×", "2×".
    static func exaggerationLabel(_ value: Double) -> String {
        value == value.rounded() ? String(format: "%.0f×", value) : String(format: "%.1f×", value)
    }

    static func exaggerationTitle(_ value: Double) -> String {
        value == 1 ? "True Height (1×)" : "\(exaggerationLabel(value)) Height"
    }

    // MARK: - Status

    @ViewBuilder
    private var status: some View {
        switch session.phase {
        case .building:
            Terrain3DLoadingCard(progress: session.progress)
                .transition(.move(edge: .bottom).combined(with: .opacity))
        case .failed(let message):
            Terrain3DNoticeCard(symbol: "exclamationmark.triangle.fill", title: message, detail: nil,
                                onRetry: { session.retry(app: model) })
        case .ready:
            if let built = session.model {
                VStack(spacing: 10) {
                    terrainNotice(built.terrain)
                    infoCapsule(built)
                }
            }
        }
    }

    @ViewBuilder
    private func terrainNotice(_ state: Terrain3DTerrainState) -> some View {
        let online = NetworkPathObserver.shared.isOnline
        switch state {
        case .complete:
            EmptyView()
        case .partial(let share):
            Terrain3DNoticeCard(symbol: "square.dashed",
                                title: "Part of the terrain isn't on your iPhone yet",
                                detail: "\(Int((share * 100).rounded())) % filled in from around it.",
                                onRetry: { session.retry(app: model) })
        case .missing:
            Terrain3DNoticeCard(symbol: online ? "mountain.2" : "wifi.slash",
                                title: "Terrain for this area isn't on your iPhone yet",
                                detail: online ? "It couldn't be downloaded just now. Showing the map flat."
                                    : "Showing the map flat. Download the area in Settings → Offline Terrain, or retry with signal.",
                                onRetry: { session.retry(app: model) })
        case .unavailable(let message):
            Terrain3DNoticeCard(symbol: "exclamationmark.triangle.fill",
                                title: "The terrain couldn't be built",
                                detail: "\(message). Showing the map flat.",
                                onRetry: { session.retry(app: model) })
        }
    }

    /// The terrain's range and the exaggeration ("1 210 – 1 690 m · height 1.5×").
    private func infoCapsule(_ built: Terrain3DModel) -> some View {
        let units = model.settings.units
        var parts: [String] = []
        if let low = built.minElevationM, let high = built.maxElevationM {
            parts.append("\(GeoFormat.elevation(low, units: units)) – \(GeoFormat.elevation(high, units: units))")
        } else {
            parts.append("Flat")
        }
        parts.append("height \(Self.exaggerationLabel(model.settings.terrainExaggeration))")
        if let interval = built.contourIntervalM {
            parts.append("contours \(GeoFormat.elevation(interval, units: units))")
        }
        return Label(parts.joined(separator: " · "), systemImage: "mountain.2.fill")
            .font(.footnote.weight(.semibold))
            .foregroundStyle(.primary)
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .floatingChrome(Capsule())
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("terrain3DInfo")
    }
}

/// The scene with what it shows, kept apart so a compass or GPS update re-renders only this.
@MainActor
private struct Terrain3DLiveScene: View {
    let session: Terrain3DSession
    let controller: Terrain3DController
    let mapName: String
    @Environment(AppModel.self) private var model

    var body: some View {
        let settings = model.settings
        let battery = model.battery
        Terrain3DSceneView(
            inputs: Terrain3DSceneInputs(
                model: session.model,
                exaggeration: settings.terrainExaggeration,
                you: model.location.location.map { LatLon($0.coordinate) },
                heading: model.location.headingDegrees,
                track: settings.showTrack ? model.tracks.current.points : [],
                items: session.items,
                itemsRevision: session.itemsRevision,
                // 60 fps and 4× MSAA; the battery saver's 30 fps without (docs/design/explorer.md).
                maxFrameRate: min(60, Int(battery.maxFrameRate.rounded())),
                antialiasing: !battery.isActive
            ),
            controller: controller,
            accessibilityName: "3D view of \(mapName)"
        )
    }
}

/// The compass: turns with the camera; a tap faces north.
@MainActor
struct Terrain3DCompassButton: View {
    let controller: Terrain3DController

    var body: some View {
        let heading = controller.headingDegrees
        Button {
            controller.faceNorth()
        } label: {
            Terrain3DCompassFace()
                .rotationEffect(.degrees(-heading))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.9))
        .floatingChrome(Circle(), interactive: true)
        .accessibilityLabel("Compass, facing \(Geodesy.compassPoint(heading)). Tap to face north")
        .accessibilityIdentifier("terrain3DCompass")
    }
}

/// A dial with a red north needle and an N, north up (the caller turns it).
private struct Terrain3DCompassFace: View {
    var body: some View {
        Canvas { context, size in
            let c = CGPoint(x: size.width / 2, y: size.height / 2)
            let r = min(size.width, size.height) / 2
            for i in 0..<12 where i % 3 != 0 {
                let a = Double(i) / 12 * 2 * .pi
                var tick = Path()
                tick.move(to: CGPoint(x: c.x + CGFloat(sin(a)) * (r - 7), y: c.y - CGFloat(cos(a)) * (r - 7)))
                tick.addLine(to: CGPoint(x: c.x + CGFloat(sin(a)) * (r - 4), y: c.y - CGFloat(cos(a)) * (r - 4)))
                context.stroke(tick, with: .color(Color.primary.opacity(0.35)), lineWidth: 1)
            }
            var north = Path()
            north.move(to: CGPoint(x: c.x, y: c.y - r + 5))
            north.addLine(to: CGPoint(x: c.x + 4, y: c.y))
            north.addLine(to: CGPoint(x: c.x - 4, y: c.y))
            north.closeSubpath()
            context.fill(north, with: .color(Theme.userRed))
            var south = Path()
            south.move(to: CGPoint(x: c.x, y: c.y + r - 5))
            south.addLine(to: CGPoint(x: c.x + 4, y: c.y))
            south.addLine(to: CGPoint(x: c.x - 4, y: c.y))
            south.closeSubpath()
            context.fill(south, with: .color(Color.primary.opacity(0.55)))
        }
        .overlay(alignment: .top) {
            Text("N")
                .font(.system(size: 8, weight: .heavy))
                .foregroundStyle(Theme.userRed)
                .offset(y: 1)
        }
        .accessibilityHidden(true)
    }
}

/// Building: a ring filling up and what is happening.
private struct Terrain3DLoadingCard: View {
    let progress: Terrain3DProgress

    var body: some View {
        let fraction = min(max(progress.fraction, 0.03), 1)
        HStack(spacing: 14) {
            ZStack {
                Circle()
                    .stroke(Color.primary.opacity(0.15), lineWidth: 4)
                Circle()
                    .trim(from: 0, to: CGFloat(fraction))
                    .stroke(Theme.accent, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.easeOut(duration: 0.3), value: fraction)
            }
            .frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(progress.step.label)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                Text(progress.tiles > 0 ? "\(progress.tiles) terrain tiles" : "The map and the terrain around it")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(14)
        .frame(maxWidth: 420)
        .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(progress.step.label) \(Int((fraction * 100).rounded())) percent")
        .accessibilityIdentifier("terrain3DLoading")
    }
}

/// Something the user should know about the terrain, with Retry.
private struct Terrain3DNoticeCard: View {
    let symbol: String
    let title: String
    let detail: String?
    var onRetry: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.warn)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
            Button("Retry", action: onRetry)
                .font(.subheadline.weight(.bold))
                .foregroundStyle(Theme.accent)
                .frame(minWidth: 44, minHeight: 44)
                .accessibilityIdentifier("terrain3DRetry")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: 520)
        .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("terrain3DNotice")
    }
}
