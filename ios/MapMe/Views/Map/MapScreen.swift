import CoreLocation
import SwiftUI
import UIKit

/// The live map, in the style of Apple Maps and AllTrails: the paper map on Apple dark grey with
/// the location puck, heading beam and dotted trail; glass controls (back, title, layers,
/// tracking, compass) and a draggable stats panel with the record button.
@MainActor
struct MapScreen: View {
    let mapID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var tileStore: TileStore?
    @State private var tracking: CanvasTrackingMode = .follow
    @State private var didSetUp = false
    @State private var controller = MapCanvasController()
    @State private var selectedPOIID: UUID?
    @State private var showNearby = false
    @State private var showCalibration = false
    @State private var showGeorefInfo = false
    @State private var panelExpanded = false
    @State private var bottomChromeHeight: CGFloat = 150
    @State private var waypointDrops = 0

    var body: some View {
        Group {
            if let map = model.library.map(id: mapID) {
                content(map: map)
            } else {
                missingMap
            }
        }
        .background(Theme.mapBackground.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .onAppear {
            model.library.touch(mapID)
            if !didSetUp {
                didSetUp = true
                tracking = model.settings.orientationMode == .headingUp ? .followWithHeading : .follow
            }
            if tileStore == nil, let map = model.library.map(id: mapID) {
                tileStore = TileStore(map: map, library: model.library)
            }
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
            let bottomGap = max(8, safe.bottom - 16)
            ZStack {
                if let tileStore {
                    LiveMapCanvas(
                        map: map,
                        tileStore: tileStore,
                        tracking: $tracking,
                        controller: controller,
                        selectedPOIID: selectedPOIID,
                        chromeInsets: UIEdgeInsets(top: safe.top + 56, left: safe.leading + 8,
                                                   bottom: bottomGap + bottomChromeHeight + 8, right: safe.trailing + 8),
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
                    Theme.mapBackground.ignoresSafeArea()
                }

                chrome(map: map, safe: safe, size: geo.size, bottomGap: bottomGap)
                    .ignoresSafeArea()
            }
        }
        .sheet(isPresented: $showNearby) {
            NearbySheet(mapID: mapID) { poi in
                showNearby = false
                tracking = .none
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    panelExpanded = false
                    selectedPOIID = poi.id
                }
            }
            .environment(model)
        }
        .sheet(isPresented: $showCalibration) {
            CalibrationView(mapID: mapID, onDone: { showCalibration = false })
                .environment(model)
        }
    }

    // MARK: - Chrome

    /// Laid out over the whole screen (safe area applied by hand, so the panel can sit low like a
    /// floating sheet).
    @ViewBuilder
    private func chrome(map: MapDocument, safe: EdgeInsets, size: CGSize, bottomGap: CGFloat) -> some View {
        let fullHeight = size.height + safe.top + safe.bottom
        VStack(spacing: 0) {
            topBar(map: map)
                .padding(.top, safe.top + 4)
                .padding(.leading, safe.leading + 12)
                .padding(.trailing, safe.trailing + 12)
            Spacer(minLength: 0)
            bottomStack(map: map, maxDetails: max(160, fullHeight * 0.72 - 180 - safe.top))
                .padding(.leading, safe.leading + 8)
                .padding(.trailing, safe.trailing + 8)
                .padding(.bottom, bottomGap)
        }
    }

    private func topBar(map: MapDocument) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left")
            }
            .buttonStyle(MapRoundButtonStyle())
            .accessibilityLabel("Back")
            .accessibilityIdentifier("backButton")

            MapTitleCapsule(map: map, showInfo: $showGeorefInfo, onLocateAgain: {
                // Present the next screen only after the popover has finished dismissing.
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

            Spacer(minLength: 0)

            VStack(spacing: 10) {
                CanvasControlStack(map: map, tracking: $tracking)
                if controller.isRotated {
                    CanvasCompassButton(controller: controller)
                        .transition(.scale(scale: 0.6).combined(with: .opacity))
                }
            }
            .animation(.spring(response: 0.35, dampingFraction: 0.8), value: controller.isRotated)
        }
    }

    private func bottomStack(map: MapDocument, maxDetails: CGFloat) -> some View {
        VStack(spacing: 10) {
            if map.georef == nil {
                NotLocatedBanner(onCalibrate: { showCalibration = true },
                                 onRetry: { model.relocate(mapID: mapID) })
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if let id = selectedPOIID, let poi = map.pois.first(where: { $0.id == id }) {
                SelectedPOICard(poi: poi) {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = nil }
                }
                .frame(maxWidth: 520)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            TrailStatsPanel(map: map, expanded: $panelExpanded, maxDetailsHeight: maxDetails, onShowAll: {
                showNearby = true
            }, onSelectPOI: { poi in
                tracking = .none
                withAnimation(.spring(response: 0.38, dampingFraction: 0.86)) {
                    panelExpanded = false
                    selectedPOIID = poi.id
                }
            })
        }
        .frame(maxWidth: .infinity)
        .background {
            GeometryReader { proxy in
                Color.clear
                    .onAppear { noteBottomChrome(proxy.size.height) }
                    .onChange(of: proxy.size.height) { _, height in noteBottomChrome(height) }
            }
        }
    }

    /// Only the collapsed layout moves the map's content area; the expanded panel floats over it.
    private func noteBottomChrome(_ height: CGFloat) {
        guard !panelExpanded, height > 0, abs(height - bottomChromeHeight) > 1 else { return }
        bottomChromeHeight = height
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
    @Binding var tracking: CanvasTrackingMode
    let controller: MapCanvasController
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
            userLocation: location.location,
            heading: location.headingDegrees,
            track: settings.showTrack ? model.tracks.current.points : [],
            showTrack: settings.showTrack,
            pois: map.pois,
            showPOIs: settings.showPOIs,
            selectedPOIID: selectedPOIID,
            markers: [],
            follow: .constant(tracking.follows),
            onTap: onTap,
            onLongPress: onLongPress,
            onSelectPOI: onSelectPOI,
            headingAccuracy: location.headingAccuracy,
            units: settings.units,
            chromeInsets: chromeInsets,
            tracking: $tracking,
            allowRotation: settings.allowMapRotation,
            locationColor: settings.locationColor,
            showMapLabels: settings.showMapLabels,
            controller: controller
        )
    }
}
