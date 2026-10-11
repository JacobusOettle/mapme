import CoreLocation
import SwiftUI
import UIKit

/// The live map: tiled map canvas, red GPS dot + heading, dotted trail, POIs, HUD and controls.
@MainActor
struct MapScreen: View {
    let mapID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var tileStore: TileStore?
    @State private var follow = true
    @State private var selectedPOIID: UUID?
    @State private var showNearby = false
    @State private var showCalibration = false
    @State private var showGeorefInfo = false
    @State private var infoExpanded = true
    @State private var waypointDrops = 0

    var body: some View {
        Group {
            if let map = model.library.map(id: mapID) {
                content(map: map)
            } else {
                missingMap
            }
        }
        .background(Theme.background.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .onAppear {
            model.library.touch(mapID)
            if tileStore == nil, let map = model.library.map(id: mapID) {
                tileStore = TileStore(map: map, library: model.library)
            }
            if model.settings.orientationMode == .headingUp { follow = true }
            UIApplication.shared.isIdleTimerDisabled = model.settings.keepScreenOn
        }
        .onDisappear {
            UIApplication.shared.isIdleTimerDisabled = false
        }
        .onChange(of: model.settings.keepScreenOn) { _, keepOn in
            UIApplication.shared.isIdleTimerDisabled = keepOn
        }
        .sensoryFeedback(.success, trigger: waypointDrops)
    }

    @ViewBuilder
    private func content(map: MapDocument) -> some View {
        GeometryReader { geo in
            let safe = geo.safeAreaInsets
            ZStack {
                if let tileStore {
                    LiveMapCanvas(
                        map: map,
                        tileStore: tileStore,
                        follow: $follow,
                        selectedPOIID: selectedPOIID,
                        chromeInsets: UIEdgeInsets(top: safe.top + 64, left: safe.leading + 8,
                                                   bottom: safe.bottom + 92, right: safe.trailing + 62),
                        onSelectPOI: { poi in
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                selectedPOIID = selectedPOIID == poi.id ? nil : poi.id
                            }
                        },
                        onTap: { _ in
                            guard selectedPOIID != nil else { return }
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = nil }
                        },
                        onLongPress: { pixel in dropWaypoint(at: pixel) }
                    )
                    .ignoresSafeArea()
                } else {
                    Theme.background.ignoresSafeArea()
                }

                chrome(map: map)
            }
        }
        .sheet(isPresented: $showNearby) {
            NearbySheet(mapID: mapID) { poi in
                showNearby = false
                follow = false
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = poi.id }
            }
            .presentationDetents([.medium, .large])
            .presentationBackground(.ultraThinMaterial)
            .environment(model)
        }
        .sheet(isPresented: $showCalibration) {
            CalibrationView(mapID: mapID, onDone: { showCalibration = false })
                .environment(model)
        }
    }

    // MARK: - Chrome

    @ViewBuilder
    private func chrome(map: MapDocument) -> some View {
        VStack(spacing: 10) {
            topBar(map: map)
            HStack(alignment: .top, spacing: 10) {
                MapInfoCard(map: map, expanded: $infoExpanded)
                Spacer(minLength: 0)
                sideControls
            }
            Spacer(minLength: 0)
            if map.georef == nil {
                NotLocatedBanner(onCalibrate: { showCalibration = true },
                                 onRetry: { model.relocate(mapID: mapID) })
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if let id = selectedPOIID, let poi = map.pois.first(where: { $0.id == id }) {
                SelectedPOICard(poi: poi) {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = nil }
                }
                .frame(maxWidth: 420)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            bottomBar
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .padding(.bottom, 8)
    }

    private func topBar(map: MapDocument) -> some View {
        HStack(spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left")
            }
            .buttonStyle(MapRoundButtonStyle())
            .accessibilityLabel("Back")
            .accessibilityIdentifier("backButton")

            VStack(alignment: .leading, spacing: 3) {
                Text(map.name)
                    .font(.system(.headline, design: .rounded))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                Button {
                    showGeorefInfo = true
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: MapStatus.symbol(for: map))
                        Text(GeorefLabels.chipText(map.georef))
                            .monospacedDigit()
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(MapStatus.color(for: map))
                }
                .accessibilityIdentifier("georefChip")
                .popover(isPresented: $showGeorefInfo, arrowEdge: .top) {
                    // Present the next screen only after the popover has finished dismissing.
                    GeorefInfoView(map: map, onLocateAgain: {
                        showGeorefInfo = false
                        Task { @MainActor in
                            try? await Task.sleep(nanoseconds: 450_000_000)
                            model.relocate(mapID: mapID)
                        }
                    }, onCalibrate: {
                        showGeorefInfo = false
                        Task { @MainActor in
                            try? await Task.sleep(nanoseconds: 450_000_000)
                            showCalibration = true
                        }
                    })
                    .presentationCompactAdaptation(.popover)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
            .frame(maxWidth: 280, alignment: .leading)
            .glass(cornerRadius: 18)
            Spacer(minLength: 0)
        }
    }

    private var sideControls: some View {
        @Bindable var settings = model.settings
        return VStack(spacing: 10) {
            OrientationButton()
                .onChange(of: settings.orientationMode) { _, mode in
                    if mode == .headingUp { follow = true }
                }

            Button {
                withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) { follow = true }
            } label: {
                Image(systemName: follow ? "location.fill" : "location")
                    .contentTransition(.symbolEffect(.replace))
            }
            .buttonStyle(MapRoundButtonStyle(active: follow))
            .accessibilityLabel(follow ? "Following your position" : "Center on my position")
            .accessibilityIdentifier("recenterButton")
            .sensoryFeedback(.impact(weight: .light), trigger: follow) { _, new in new }

            Menu {
                Toggle(isOn: $settings.nightMap) {
                    Label("Night map", systemImage: "moon.fill")
                }
                Toggle(isOn: $settings.showTrack) {
                    Label("Show trail", systemImage: "figure.walk")
                }
                Toggle(isOn: $settings.showPOIs) {
                    Label("Points of interest", systemImage: "mappin.and.ellipse")
                }
                Divider()
                Button(role: .destructive) {
                    model.tracks.clearCurrent()
                } label: {
                    Label("Clear trail", systemImage: "trash")
                }
            } label: {
                Image(systemName: "square.3.layers.3d")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .frame(width: 46, height: 46)
                    .glass(cornerRadius: 23)
                    .shadow(color: .black.opacity(0.35), radius: 8, y: 3)
            }
            .accessibilityLabel("Map layers")
            .accessibilityIdentifier("layersMenu")
        }
    }

    private var bottomBar: some View {
        HStack(spacing: 12) {
            RecordButton()
            Button {
                showNearby = true
            } label: {
                Label("Nearby", systemImage: "mappin.and.ellipse")
            }
            .buttonStyle(MapCapsuleButtonStyle())
            .accessibilityIdentifier("nearbyButton")
        }
        .frame(maxWidth: .infinity)
    }

    private var missingMap: some View {
        VStack(spacing: 14) {
            Image(systemName: "map")
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(Theme.textSecondary)
            Text("This map is no longer in your library.")
                .foregroundStyle(Theme.textSecondary)
            Button("Back") { dismiss() }
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Actions

    /// Long-press drops a user waypoint at that spot on the map.
    private func dropWaypoint(at pixel: CGPoint) {
        guard var map = model.library.map(id: mapID), let georef = map.georef,
              pixel.x >= 0, pixel.y >= 0, pixel.x <= CGFloat(map.pixelWidth), pixel.y <= CGFloat(map.pixelHeight)
        else { return }
        let n = map.pois.filter { $0.source == .user && $0.kind == .waypoint }.count + 1
        let poi = MapPOI(name: "Waypoint \(n)", kind: .waypoint, source: .user,
                         position: georef.latLon(forPixel: pixel), elevationM: nil)
        map.pois.append(poi)
        model.library.save(map)
        waypointDrops += 1
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = poi.id }
    }
}

/// The canvas plus the fast-changing inputs (location, heading, trail). Kept separate so a compass
/// update re-renders only this view, not the whole screen.
@MainActor
private struct LiveMapCanvas: View {
    let map: MapDocument
    let tileStore: TileStore
    @Binding var follow: Bool
    let selectedPOIID: UUID?
    let chromeInsets: UIEdgeInsets
    var onSelectPOI: (MapPOI) -> Void
    var onTap: (CGPoint) -> Void
    var onLongPress: (CGPoint) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let settings = model.settings
        let location = model.location
        MapCanvasView(
            map: map,
            tileStore: tileStore,
            nightMode: settings.nightMap,
            orientation: settings.orientationMode,
            userLocation: location.location,
            heading: location.headingDegrees,
            track: settings.showTrack ? model.tracks.current.points : [],
            showTrack: settings.showTrack,
            pois: map.pois,
            showPOIs: settings.showPOIs,
            selectedPOIID: selectedPOIID,
            markers: [],
            follow: $follow,
            onTap: onTap,
            onLongPress: onLongPress,
            onSelectPOI: onSelectPOI,
            headingAccuracy: location.headingAccuracy,
            units: settings.units,
            chromeInsets: chromeInsets
        )
    }
}
