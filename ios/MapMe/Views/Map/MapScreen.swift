import CoreLocation
import SwiftUI
import UIKit

/// The live map, in the style of Apple Maps and AllTrails: the paper map on Apple dark grey with
/// the location puck, heading beam and dotted trail; glass controls (back, title, layers,
/// tracking, compass, then the features' side column) and a draggable stats panel with the record
/// button. The explorer features plug in through `MapScreenContext` (the map shown, selection,
/// tool, presentations), `CanvasOverlaySet` (canvas content), `MapSideColumn` and `MapTopBanners`
/// (docs/design/explorer.md, Map screen).
@MainActor
struct MapScreen: View {
    /// The map opened (auto-switching may then show another in place: `context.mapID`).
    let mapID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var context: MapScreenContext
    @State private var tracking: CanvasTrackingMode = .follow
    @State private var didSetUp = false
    @State private var selectedPOIID: UUID?
    @State private var showNearby = false
    @State private var showCalibration = false
    @State private var showGeorefInfo = false
    @State private var panelExpanded = false
    @State private var bottomChromeHeight: CGFloat = 150
    @State private var pinDrops = 0

    init(mapID: UUID) {
        self.mapID = mapID
        _context = State(initialValue: MapScreenContext(mapID: mapID))
    }

    var body: some View {
        Group {
            if let map = model.library.map(id: context.mapID) {
                content(map: map)
            } else {
                missingMap
            }
        }
        .background(Theme.mapBackground.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .onAppear {
            context.attach(model)
            model.library.touch(context.mapID)
            if !didSetUp {
                didSetUp = true
                tracking = model.settings.orientationMode == .headingUp ? .followWithHeading : .follow
                context.tracking = tracking
            }
            updateIdleTimer()
        }
        .onDisappear {
            UIApplication.shared.isIdleTimerDisabled = false
        }
        .onChange(of: model.settings.keepScreenOn) { _, _ in updateIdleTimer() }
        .onChange(of: model.battery.allowsKeepScreenAwake) { _, _ in updateIdleTimer() }
        .onChange(of: tracking) { _, mode in context.tracking = mode }
        .onChange(of: context.mapID) { _, id in mapDidSwitch(to: id) }
        .sensoryFeedback(.success, trigger: pinDrops)
    }

    @ViewBuilder
    private func content(map: MapDocument) -> some View {
        // Bindings for the features' presentations only; closures below use `context` itself (a
        // possibly @Sendable closure must not capture the local @Bindable var).
        @Bindable var presenting = context
        // SwiftUI's safe area, raised to the window's where it falls short (a screen laid out
        // while a cover was still closing), so the controls never slide under the status bar.
        SafeAreaReader { size, safe in
            let bottomGap = max(8, safe.bottom - 16)
            ZStack {
                // The tiles of the map shown (switching maps waits for the new map's tiles).
                if let tileStore = context.tileStore, tileStore.map.id == map.id {
                    LiveMapCanvas(
                        map: map,
                        tileStore: tileStore,
                        tracking: $tracking,
                        context: context,
                        selectedPOIID: selectedPOIID,
                        chromeInsets: UIEdgeInsets(top: safe.top + 56, left: safe.leading + 8,
                                                   bottom: bottomGap + bottomChromeHeight + 8, right: safe.trailing + 8),
                        onSelectPOI: { poi in
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                context.clearSelection()
                                selectedPOIID = selectedPOIID == poi.id ? nil : poi.id
                            }
                        },
                        onSelectHit: { hit in select(hit) },
                        onTap: { _ in
                            guard selectedPOIID != nil || context.selectedItem != nil || context.selectedForeignPOI != nil
                                || context.droppedPin != nil else { return }
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                selectedPOIID = nil
                                context.clearSelection()
                            }
                        },
                        onLongPress: { pixel in dropPin(at: pixel, map: map) }
                    )
                    .ignoresSafeArea()
                } else {
                    Theme.mapBackground.ignoresSafeArea()
                }

                chrome(map: map, safe: safe, size: size, bottomGap: bottomGap)
                    .ignoresSafeArea()
            }
        }
        .sheet(isPresented: $showNearby) {
            NearbySheet(mapID: context.mapID) { poi in
                showNearby = false
                tracking = .none
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    panelExpanded = false
                    context.clearSelection()
                    selectedPOIID = poi.id
                }
            }
            .environment(model)
        }
        .sheet(isPresented: $showCalibration) {
            CalibrationView(mapID: context.mapID, onDone: { showCalibration = false })
                .environment(model)
        }
        .sheet(item: $presenting.sheet) { presentation in
            presentation.content()
                .environment(model)
        }
        .fullScreenCover(item: $presenting.cover) { presentation in
            presentation.content()
                .environment(model)
        }
    }

    // MARK: - Chrome

    /// Laid out over the whole screen (safe area applied by hand, so the panel can sit low like a
    /// floating sheet). The top bar and the bottom panel are pinned to their edges rather than
    /// stacked, so a tall bottom panel never pushes the top bar under the status bar; the top
    /// bar's columns get the height left above the panel (`MapChromeLayout`) and the expanded
    /// panel floats over them.
    @ViewBuilder
    private func chrome(map: MapDocument, safe: EdgeInsets, size: CGSize, bottomGap: CGFloat) -> some View {
        let fullHeight = size.height + safe.top + safe.bottom
        let layout = MapChromeLayout(screen: CGSize(width: size.width + safe.leading + safe.trailing, height: fullHeight),
                                     safe: safe, bottomGap: bottomGap, bottomPanelHeight: bottomChromeHeight,
                                     compassShown: context.controller.isRotated)
        ZStack {
            topBar(map: map, layout: layout)
                .padding(.top, layout.top)
                .padding(.leading, safe.leading + 12)
                .padding(.trailing, safe.trailing + 12)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            bottomStack(map: map, maxDetails: max(160, fullHeight * 0.72 - 180 - safe.top))
                .padding(.leading, safe.leading + 8)
                .padding(.trailing, safe.trailing + 8)
                .padding(.bottom, bottomGap)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
        }
    }

    private func topBar(map: MapDocument, layout: MapChromeLayout) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left")
            }
            .buttonStyle(MapRoundButtonStyle())
            .accessibilityLabel("Back")
            .accessibilityIdentifier("backButton")

            VStack(alignment: .leading, spacing: 8) {
                MapTitleCapsule(map: map, showInfo: $showGeorefInfo, onLocateAgain: {
                    // Present the next screen only after the popover has finished dismissing.
                    showGeorefInfo = false
                    let id = context.mapID
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 450_000_000)
                        model.relocate(mapID: id)
                    }
                }, onCalibrate: {
                    showGeorefInfo = false
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 450_000_000)
                        showCalibration = true
                    }
                })
                MapTopBanners(context: context, map: map, maxHeight: layout.bannersMaxHeight)
            }

            Spacer(minLength: 0)

            VStack(alignment: .trailing, spacing: MapChromeLayout.spacing) {
                CanvasControlStack(map: map, tracking: $tracking)
                if context.controller.isRotated {
                    CanvasCompassButton(controller: context.controller)
                        .transition(.scale(scale: 0.6).combined(with: .opacity))
                }
                MapSideColumn(context: context, map: map, maxHeight: layout.sideColumnMaxHeight)
            }
            .animation(.spring(response: 0.35, dampingFraction: 0.8), value: context.controller.isRotated)
        }
    }

    private func bottomStack(map: MapDocument, maxDetails: CGFloat) -> some View {
        VStack(spacing: 10) {
            if map.georef == nil {
                NotLocatedBanner(onCalibrate: { showCalibration = true },
                                 onRetry: { model.relocate(mapID: context.mapID) })
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            selectionCard(map: map)
            if let tool = context.tool {
                tool.makeBar()
                    .frame(maxWidth: 520)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            } else {
                TrailStatsPanel(map: map, expanded: $panelExpanded, maxDetailsHeight: maxDetails, onShowAll: {
                    openNearby()
                }, onSelectPOI: { poi in
                    tracking = .none
                    withAnimation(.spring(response: 0.38, dampingFraction: 0.86)) {
                        panelExpanded = false
                        context.clearSelection()
                        selectedPOIID = poi.id
                    }
                })
            }
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

    /// The card of what is selected: the dropped pin, a geo item, another map's place, or one of
    /// this map's POIs.
    @ViewBuilder
    private func selectionCard(map: MapDocument) -> some View {
        if let pin = context.droppedPin {
            DroppedPinCard(pin: pin, context: context, map: map)
                .modifier(BottomCardPlacement())
        } else if let ref = context.selectedItem {
            SelectedGeoItemCard(ref: ref, context: context, map: map)
                .modifier(BottomCardPlacement())
        } else if let poi = context.selectedForeignPOI {
            SelectedPOICard(poi: poi) {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { context.selectedForeignPOI = nil }
            }
            .modifier(BottomCardPlacement())
        } else if let id = selectedPOIID, let poi = map.pois.first(where: { $0.id == id }) {
            SelectedPOICard(poi: poi) {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { selectedPOIID = nil }
            }
            .modifier(BottomCardPlacement())
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

    /// "Show All" in the expanded stats panel: collapse the panel first so it does not peek out
    /// above the Nearby sheet (with a second grabber), then present the sheet.
    private func openNearby() {
        guard panelExpanded else {
            showNearby = true
            return
        }
        withAnimation(.spring(response: 0.3, dampingFraction: 0.9)) {
            panelExpanded = false
        }
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 220_000_000)
            showNearby = true
        }
    }

    /// A tap on a feature's content: a geo item's card, or another map's place.
    private func select(_ hit: CanvasHit) {
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
            selectedPOIID = nil
            context.droppedPin = nil
            context.selectedItem = hit.item
            context.selectedForeignPOI = hit.item == nil ? hit.poi : nil
        }
    }

    /// Long-press drops a pin; its card offers a waypoint, a note, a photo or a route there.
    private func dropPin(at pixel: CGPoint, map: MapDocument) {
        guard let georef = map.georef else { return }
        let m = MapContinuation.marginPixels(for: map, enabled: model.settings.showOffMapTerrain)
        let area = CGRect(origin: .zero, size: map.pixelSize).insetBy(dx: -m, dy: -m)
        guard area.contains(pixel) else { return }
        let position = georef.latLon(forPixel: pixel)
        guard position.lat.isFinite, position.lon.isFinite else { return }
        pinDrops += 1
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
            selectedPOIID = nil
            context.clearSelection()
            context.droppedPin = DroppedPin(position: position)
        }
    }

    /// Auto-switching or "Maps here" showed another map: it counts as opened.
    private func mapDidSwitch(to id: UUID) {
        selectedPOIID = nil
        model.library.touch(id)
    }

    private func updateIdleTimer() {
        UIApplication.shared.isIdleTimerDisabled = model.settings.keepScreenOn && model.battery.allowsKeepScreenAwake
    }
}

/// A card above the stats panel: at most 520 pt wide, sliding in from the bottom.
private struct BottomCardPlacement: ViewModifier {
    func body(content: Content) -> some View {
        content
            .frame(maxWidth: 520)
            .transition(.move(edge: .bottom).combined(with: .opacity))
    }
}

/// The canvas plus the fast-changing inputs (location, heading, trail). Kept separate so a compass
/// update re-renders only this view, not the whole screen.
@MainActor
private struct LiveMapCanvas: View {
    let map: MapDocument
    let tileStore: TileStore
    @Binding var tracking: CanvasTrackingMode
    let context: MapScreenContext
    let selectedPOIID: UUID?
    let chromeInsets: UIEdgeInsets
    var onSelectPOI: (MapPOI) -> Void
    var onSelectHit: (CanvasHit) -> Void
    var onTap: (CGPoint) -> Void
    var onLongPress: (CGPoint) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let settings = model.settings
        let location = model.location
        let battery = model.battery
        MapCanvasView(
            map: map,
            tileStore: tileStore,
            darkMap: settings.mapAppearance == .dark,
            userLocation: location.location,
            heading: location.headingDegrees,
            track: settings.showTrack ? model.tracks.current.points : [],
            showTrack: settings.showTrack,
            pois: map.pois,
            showPOIs: settings.showPOIs,
            selectedPOIID: selectedPOIID,
            markers: pinMarkers,
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
            controller: context.controller,
            featureOverlays: context.overlays?.overlays ?? [],
            mapSpaceLayers: context.overlays?.mapSpaceLayers ?? [],
            tool: context.tool,
            continuationMargin: MapContinuation.marginPixels(for: map, enabled: settings.showOffMapTerrain),
            cameraHandoff: context.pendingHandoff,
            maxFrameRate: battery.maxFrameRate,
            animationsEnabled: battery.animationsEnabled,
            onSelectHit: onSelectHit
        )
    }

    /// The long-press pin, drawn by the canvas.
    private var pinMarkers: [CanvasMarker] {
        guard let pin = context.droppedPin, let georef = map.georef else { return [] }
        let pixel = georef.pixel(for: pin.position)
        guard pixel.x.isFinite, pixel.y.isFinite else { return [] }
        return [CanvasMarker(id: "droppedPin", pixel: pixel, label: "", color: Theme.uiUserRed)]
    }
}
