import CoreLocation
import MapKit
import SwiftUI
import UIKit

/// Manual georeferencing: pair points on the map with points on a satellite map
/// (or with "I'm here" at your GPS position). Saves the fit into the map document.
struct CalibrationView: View {
    let mapID: UUID
    var onDone: () -> Void
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.verticalSizeClass) private var verticalSizeClass

    private enum Mode: Equatable {
        case points
        case pickColour
    }

    @State private var mode: Mode = .points
    @State private var pairs: [CalibrationPair] = []
    @State private var pendingPixel: CGPoint? = nil
    @State private var pendingCoordinate: LatLon? = nil
    @State private var fit: CalibrationFit? = nil
    @State private var fitError: String? = nil
    @State private var tileStore: TileStore? = nil

    @State private var camera: MapCameraPosition = .automatic
    @State private var cameraReady = false
    @State private var visibleRegion: MKCoordinateRegion? = nil
    @State private var cameraDistance: Double = 1500
    /// Where the current georeference expects the pending map point (satellite hint).
    @State private var predicted: LatLon? = nil

    /// A contour colour was picked in this session (it is saved to the map right away).
    @State private var pickedThisSession = false
    @State private var samplePixel: CGPoint? = nil
    @State private var sampling = false

    @State private var showDiscard = false
    @State private var showClear = false
    @State private var tapTick = 0
    @State private var pairTick = 0
    @State private var undoTick = 0
    @State private var pickTick = 0

    init(mapID: UUID, onDone: @escaping () -> Void) {
        self.mapID = mapID
        self.onDone = onDone
    }

    var body: some View {
        NavigationStack {
            Group {
                if let map = model.library.map(id: mapID) {
                    content(map)
                } else {
                    ContentUnavailableView("Map not found", systemImage: "map",
                                           description: Text("This map is no longer in your library."))
                }
            }
            .background(Theme.background.ignoresSafeArea())
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Theme.background, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbar { toolbarContent }
        }
        .interactiveDismissDisabled(!pairs.isEmpty)
        .sensoryFeedback(.selection, trigger: tapTick)
        .sensoryFeedback(.success, trigger: pairTick)
        .sensoryFeedback(.impact(weight: .light), trigger: undoTick)
        .sensoryFeedback(.impact(weight: .medium), trigger: pickTick)
        .confirmationDialog("Discard calibration?", isPresented: $showDiscard, titleVisibility: .visible) {
            Button("Discard", role: .destructive) { dismiss() }
            Button("Keep editing", role: .cancel) {}
        } message: {
            Text("The points you placed are not saved.")
        }
        .confirmationDialog("Clear all points?", isPresented: $showClear, titleVisibility: .visible) {
            Button("Clear \(pairs.count) points", role: .destructive) { clearAll() }
            Button("Cancel", role: .cancel) {}
        }
        .onAppear { setUp() }
    }

    // MARK: - Layout

    @MainActor
    @ViewBuilder
    private func content(_ map: MapDocument) -> some View {
        let step = currentStep()
        if verticalSizeClass == .compact {
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    mapPane(map, step: step)
                    satellitePane(step: step)
                }
                bottomPanel(map, step: step)
            }
            .padding(8)
        } else {
            VStack(spacing: 8) {
                mapPane(map, step: step)
                bottomPanel(map, step: step)
                satellitePane(step: step)
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
    }

    @MainActor
    private func mapPane(_ map: MapDocument, step: CalibrationStep) -> some View {
        ZStack(alignment: .topLeading) {
            Theme.surface
            if let tileStore {
                MapCanvasView(map: map, tileStore: tileStore, markers: canvasMarkers(map),
                              follow: .constant(false),
                              onTap: { pixel in handleMapTap(pixel, map: map) })
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            paneLabel(mode == .pickColour ? "YOUR MAP \u{00B7} PICK COLOUR" : "YOUR MAP",
                      symbol: mode == .pickColour ? "eyedropper" : "map",
                      active: step.target == .map)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay(paneBorder(active: step.target == .map))
        .accessibilityIdentifier("calibrationMapPane")
    }

    @MainActor
    private func satellitePane(step: CalibrationStep) -> some View {
        ZStack(alignment: .topLeading) {
            Theme.surface
            if cameraReady {
                CalibrationSatelliteMap(
                    camera: $camera,
                    pairs: pairs,
                    pendingCoordinate: pendingCoordinate,
                    predicted: predicted,
                    onTap: { coordinate in handleSatelliteTap(coordinate) },
                    onCameraChange: { region, distance in
                        visibleRegion = region
                        cameraDistance = distance
                    }
                )
            }
            paneLabel("SATELLITE", symbol: "globe.europe.africa.fill", active: step.target == .satellite)
        }
        .overlay(alignment: .bottom) {
            if mode == .points {
                imHereButton
                    .padding(.bottom, 12)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay(paneBorder(active: step.target == .satellite))
        .accessibilityIdentifier("calibrationSatellitePane")
    }

    @MainActor
    @ViewBuilder
    private func bottomPanel(_ map: MapDocument, step: CalibrationStep) -> some View {
        if mode == .pickColour {
            CalibrationColourPanel(
                rgb: map.contourRGB,
                isPicked: pickedThisSession,
                sampling: sampling,
                onLocate: { locateAgain() },
                onAutomatic: {
                    withAnimation(.snappy) {
                        setContourColour(nil)
                        pickedThisSession = false
                        samplePixel = nil
                    }
                },
                onDone: { withAnimation(.snappy) { mode = .points } }
            )
            .transition(.move(edge: .bottom).combined(with: .opacity))
        } else {
            CalibrationGuidanceBar(
                step: step,
                pairs: pairs,
                fit: fit,
                fitError: fitError,
                userOnMap: userOnMap(map),
                canUndo: !pairs.isEmpty || pendingPixel != nil || pendingCoordinate != nil,
                onUndo: { undo() },
                onClear: {
                    if pairs.count >= 2 { showClear = true } else { clearAll() }
                },
                onDelete: { index in deletePair(at: index) }
            )
            .transition(.opacity)
        }
    }

    private func paneLabel(_ title: String, symbol: String, active: Bool) -> some View {
        HStack(spacing: 5) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .bold))
            Text(title)
                .font(.system(size: 11, weight: .bold, design: .rounded))
                .tracking(0.6)
        }
        .foregroundStyle(active ? Color.black : Theme.textPrimary)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(active ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Material.ultraThinMaterial), in: Capsule())
        .padding(10)
        .allowsHitTesting(false)
        .animation(.snappy, value: active)
    }

    private func paneBorder(active: Bool) -> some View {
        RoundedRectangle(cornerRadius: 18, style: .continuous)
            .stroke(active ? Theme.accent : Theme.stroke, lineWidth: active ? 2 : 1)
            .animation(.easeInOut(duration: 0.25), value: active)
            .allowsHitTesting(false)
    }

    @MainActor
    private var imHereButton: some View {
        let fix = model.location.location
        let enabled = pendingPixel != nil && fix != nil
        let accuracy = fix.flatMap { $0.horizontalAccuracy >= 0 ? $0.horizontalAccuracy : nil }
        return Button {
            addHere()
        } label: {
            HStack(spacing: 7) {
                Image(systemName: "location.fill")
                    .font(.system(size: 13, weight: .bold))
                Text("I\u{2019}m here")
                    .font(.subheadline.weight(.bold))
                if let accuracy {
                    Text("\u{00B1}" + GeoFormat.distance(accuracy, units: model.settings.units))
                        .font(.caption.weight(.semibold))
                        .monospacedDigit()
                        .opacity(0.85)
                }
            }
            .foregroundStyle(Color.white)
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(Theme.userRed.opacity(enabled ? 1 : 0.4), in: Capsule())
            .overlay(Capsule().stroke(Color.white.opacity(0.35), lineWidth: 1))
            .shadow(color: Theme.userRed.opacity(enabled ? 0.5 : 0), radius: 10)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityHint("Pairs the spot you tapped on your map with your GPS position")
        .accessibilityIdentifier("imHereButton")
    }

    @MainActor
    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { cancel() }
        }
        ToolbarItem(placement: .principal) {
            VStack(spacing: 0) {
                Text("Calibrate")
                    .font(.headline)
                    .foregroundStyle(Theme.textPrimary)
                if let name = model.library.map(id: mapID)?.name {
                    Text(name)
                        .font(.caption2)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(1)
                }
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button {
                    withAnimation(.snappy) { mode = mode == .pickColour ? .points : .pickColour }
                } label: {
                    Label(colourMenuTitle, systemImage: mode == .pickColour ? "hand.tap" : "eyedropper")
                }
                Button(role: .destructive) {
                    if pairs.count >= 2 { showClear = true } else { clearAll() }
                } label: {
                    Label("Clear all points", systemImage: "trash")
                }
                .disabled(pairs.isEmpty)
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .accessibilityLabel("More")
        }
        ToolbarItem(placement: .confirmationAction) {
            Button("Save") { save() }
                .fontWeight(.semibold)
                .disabled(fit == nil)
                .accessibilityIdentifier("calibrationSave")
        }
    }

    // MARK: - Derived state

    private var colourMenuTitle: String {
        mode == .pickColour ? "Back to points" : "Pick contour colour"
    }

    @MainActor
    private func currentStep() -> CalibrationStep {
        if mode == .pickColour {
            return CalibrationStep(symbol: "eyedropper", text: "Tap a contour line on your map.", target: .map)
        }
        if pendingPixel != nil {
            let text = model.location.location == nil
                ? "Now tap the same spot on the satellite map."
                : "Now tap the same spot on the satellite map \u{2014} or press I\u{2019}m here if you are standing on it."
            return CalibrationStep(symbol: "arrow.down.circle.fill", text: text, target: .satellite)
        }
        if pendingCoordinate != nil {
            return CalibrationStep(symbol: "arrow.up.circle.fill", text: "Now tap the same spot on your map.", target: .map)
        }
        switch pairs.count {
        case 0:
            return CalibrationStep(symbol: "hand.tap.fill",
                                   text: "Tap a spot you can recognise on your map: a road junction, a dam wall, a building, or where you are standing.",
                                   target: .map)
        case 1:
            return CalibrationStep(symbol: "2.circle.fill", text: "Nice. Now a second spot, far from the first.", target: .map)
        case 2:
            return CalibrationStep(symbol: "3.circle.fill",
                                   text: "Two points give a rough fit. Add a third, spread out, to check the accuracy.",
                                   target: .map)
        default:
            if let worst = fit?.residuals.max(), worst >= 50 {
                return CalibrationStep(symbol: "exclamationmark.triangle.fill",
                                       text: "A point disagrees with the others (red). Tap its chip to delete it, or add more points.",
                                       target: .neither)
            }
            return CalibrationStep(symbol: "checkmark.seal.fill",
                                   text: "Looking good. Add more points to refine it, or Save.", target: .neither)
        }
    }

    /// The georeference used for predictions: the live fit, else the saved one.
    @MainActor
    private func activeGeoref(_ map: MapDocument) -> Georeference? {
        fit?.georef ?? map.georef
    }

    @MainActor
    private func canvasMarkers(_ map: MapDocument) -> [CanvasMarker] {
        var markers: [CanvasMarker] = []
        for (index, pair) in pairs.enumerated() {
            markers.append(CanvasMarker(id: pair.id.uuidString, pixel: pair.pixel, label: "\(index + 1)",
                                        color: CalibrationPalette.uiColor(index)))
        }
        if let pendingPixel {
            markers.append(CanvasMarker(id: "pending", pixel: pendingPixel, label: "\(pairs.count + 1)?",
                                        color: Theme.uiAccent))
        } else if let pendingCoordinate, let g = activeGeoref(map) {
            let px = g.pixel(for: pendingCoordinate)
            if Self.isNear(px, map) {
                markers.append(CanvasMarker(id: "predicted", pixel: px, label: "\u{2248}",
                                            color: Theme.uiAccent.withAlphaComponent(0.55)))
            }
        }
        if let fit, let fix = model.location.location {
            let px = fit.georef.pixel(for: LatLon(fix.coordinate))
            if Self.isNear(px, map) {
                markers.append(CanvasMarker(id: "ghost-you", pixel: px, label: "You",
                                            color: Theme.uiUserRed.withAlphaComponent(0.8)))
            }
        }
        if mode == .pickColour, let samplePixel, let rgb = map.contourRGB {
            markers.append(CanvasMarker(id: "sample", pixel: samplePixel, label: "", color: CalibrationPalette.uiSwatch(rgb)))
        }
        return markers
    }

    @MainActor
    private func userOnMap(_ map: MapDocument) -> Bool? {
        guard let fit, let fix = model.location.location else { return nil }
        let px = fit.georef.pixel(for: LatLon(fix.coordinate))
        guard px.x.isFinite, px.y.isFinite else { return nil }
        return px.x >= 0 && px.y >= 0 && px.x <= CGFloat(map.pixelWidth) && px.y <= CGFloat(map.pixelHeight)
    }

    /// Within one map size of the raster (markers further out are noise from a bad fit).
    private static func isNear(_ p: CGPoint, _ map: MapDocument) -> Bool {
        let w = CGFloat(map.pixelWidth), h = CGFloat(map.pixelHeight)
        return p.x.isFinite && p.y.isFinite && p.x > -w && p.x < 2 * w && p.y > -h && p.y < 2 * h
    }

    private static func isInside(_ p: CGPoint, _ map: MapDocument) -> Bool {
        p.x.isFinite && p.y.isFinite && p.x >= 0 && p.y >= 0
            && p.x <= CGFloat(map.pixelWidth) && p.y <= CGFloat(map.pixelHeight)
    }

    private static func isValid(_ p: LatLon) -> Bool {
        p.lat.isFinite && p.lon.isFinite && abs(p.lat) <= 85 && abs(p.lon) <= 180
    }

    // MARK: - Setup

    @MainActor
    private func setUp() {
        guard let map = model.library.map(id: mapID) else { return }
        if tileStore == nil {
            tileStore = TileStore(map: map, library: model.library)
        }
        if !cameraReady {
            camera = initialCamera(map)
            cameraReady = true
        }
    }

    /// The map's extent when it is already located, else around the user, else the world.
    @MainActor
    private func initialCamera(_ map: MapDocument) -> MapCameraPosition {
        if let g = map.georef {
            let w = CGFloat(map.pixelWidth), h = CGFloat(map.pixelHeight)
            let corners = [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)]
                .map { g.latLon(forPixel: $0) }
            if corners.allSatisfy({ Self.isValid($0) }) {
                let lats = corners.map(\.lat), lons = corners.map(\.lon)
                if let minLat = lats.min(), let maxLat = lats.max(), let minLon = lons.min(), let maxLon = lons.max(),
                   maxLat - minLat < 5, maxLon - minLon < 5 {
                    let center = CLLocationCoordinate2D(latitude: (minLat + maxLat) / 2, longitude: (minLon + maxLon) / 2)
                    let span = MKCoordinateSpan(latitudeDelta: max(0.002, (maxLat - minLat) * 1.15),
                                                longitudeDelta: max(0.002, (maxLon - minLon) * 1.15))
                    return .region(MKCoordinateRegion(center: center, span: span))
                }
            }
        }
        if let fix = model.location.location {
            return .region(MKCoordinateRegion(center: fix.coordinate, latitudinalMeters: 3000, longitudinalMeters: 3000))
        }
        let world = MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: 15, longitude: 10),
                                       span: MKCoordinateSpan(latitudeDelta: 120, longitudeDelta: 160))
        return .userLocation(fallback: .region(world))
    }

    // MARK: - Taps

    @MainActor
    private func handleMapTap(_ pixel: CGPoint, map: MapDocument) {
        guard Self.isInside(pixel, map) else { return }
        switch mode {
        case .pickColour:
            sampleColour(at: pixel, map: map)
        case .points:
            tapTick += 1
            if let coordinate = pendingCoordinate {
                addPair(pixel: pixel, coordinate: coordinate, fromGPS: false)
                return
            }
            withAnimation(.snappy) { pendingPixel = pixel }
            if let g = activeGeoref(map) {
                let guess = g.latLon(forPixel: pixel)
                if Self.isValid(guess) {
                    predicted = guess
                    focusSatellite(on: guess)
                } else {
                    predicted = nil
                }
            }
        }
    }

    @MainActor
    private func handleSatelliteTap(_ coordinate: CLLocationCoordinate2D) {
        guard mode == .points else { return }
        let point = LatLon(coordinate)
        guard Self.isValid(point) else { return }
        tapTick += 1
        if let pixel = pendingPixel {
            addPair(pixel: pixel, coordinate: point, fromGPS: false)
        } else {
            withAnimation(.snappy) { pendingCoordinate = point }
        }
    }

    /// Bring the predicted spot into view unless it is already well inside the visible area.
    @MainActor
    private func focusSatellite(on point: LatLon) {
        let c = point.coordinate
        if let r = visibleRegion,
           abs(c.latitude - r.center.latitude) < r.span.latitudeDelta * 0.35,
           abs(c.longitude - r.center.longitude) < r.span.longitudeDelta * 0.35 {
            return
        }
        let distance = min(max(cameraDistance, 500), 4000)
        withAnimation(.easeInOut(duration: 0.6)) {
            camera = .camera(MapCamera(centerCoordinate: c, distance: distance))
        }
    }

    // MARK: - Editing

    @MainActor
    private func addPair(pixel: CGPoint, coordinate: LatLon, fromGPS: Bool) {
        withAnimation(.snappy) {
            pairs.append(CalibrationPair(pixel: pixel, coordinate: coordinate, fromGPS: fromGPS))
            pendingPixel = nil
            pendingCoordinate = nil
            predicted = nil
        }
        pairTick += 1
        refit()
    }

    @MainActor
    private func addHere() {
        guard let pixel = pendingPixel, let fix = model.location.location else { return }
        addPair(pixel: pixel, coordinate: LatLon(fix.coordinate), fromGPS: true)
    }

    @MainActor
    private func undo() {
        undoTick += 1
        withAnimation(.snappy) {
            if pendingPixel != nil || pendingCoordinate != nil {
                pendingPixel = nil
                pendingCoordinate = nil
                predicted = nil
            } else if !pairs.isEmpty {
                pairs.removeLast()
            }
        }
        refit()
    }

    @MainActor
    private func deletePair(at index: Int) {
        guard pairs.indices.contains(index) else { return }
        undoTick += 1
        withAnimation(.snappy) { _ = pairs.remove(at: index) }
        refit()
    }

    @MainActor
    private func clearAll() {
        undoTick += 1
        withAnimation(.snappy) {
            pairs.removeAll()
            pendingPixel = nil
            pendingCoordinate = nil
            predicted = nil
        }
        refit()
    }

    @MainActor
    private func refit() {
        guard pairs.count >= 2 else {
            fit = nil
            fitError = nil
            return
        }
        switch CalibrationFit.make(from: pairs) {
        case .success(let result):
            fit = result
            fitError = nil
        case .failure(let error):
            fit = nil
            fitError = error.message
        }
    }

    // MARK: - Contour colour

    @MainActor
    private func sampleColour(at pixel: CGPoint, map: MapDocument) {
        guard let store = tileStore, !sampling else { return }
        sampling = true
        samplePixel = pixel
        let tileSize = map.tileSize
        Task { @MainActor in
            let rgb = await Task.detached(priority: .userInitiated) { () -> [Int]? in
                ContourColorSampler.sample(store: store, pixel: pixel, tileSize: tileSize)
            }.value
            sampling = false
            if let rgb {
                pickTick += 1
                withAnimation(.snappy) {
                    setContourColour(rgb)
                    pickedThisSession = true
                }
            }
        }
    }

    /// MapDocument.contourRGB is what the locator uses; nil = built-in contour colours.
    @MainActor
    private func setContourColour(_ rgb: [Int]?) {
        guard var doc = model.library.map(id: mapID), doc.contourRGB != rgb else { return }
        doc.contourRGB = rgb
        model.library.save(doc)
    }

    /// Close and run the automatic locator again with the saved contour colour.
    @MainActor
    private func locateAgain() {
        guard model.library.map(id: mapID) != nil else { return }
        let appModel = model
        let id = mapID
        onDone()
        // Let this sheet (and, during an import, the progress screen) finish dismissing
        // before the progress screen is presented again for the new run.
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 900_000_000)
            appModel.relocate(mapID: id)
        }
    }

    // MARK: - Save / cancel

    @MainActor
    private func save() {
        guard let fit, var doc = model.library.map(id: mapID) else { return }
        let rms = fit.rms
        let georef = Georeference(pixelToMercator: fit.georef.pixelToMercator, method: .manual,
                                  confidence: CalibrationFit.confidence(forRMS: rms), rmsMeters: rms)
        // Labels read off the map belong to map pixels: carry them over to the new fit.
        if let old = doc.georef {
            doc.pois = doc.pois.map { poi in
                guard poi.source == .mapLabel else { return poi }
                let px = old.pixel(for: poi.position)
                guard px.x.isFinite, px.y.isFinite else { return poi }
                var moved = poi
                moved.position = georef.latLon(forPixel: px)
                return moved
            }
        }
        doc.georef = georef
        doc.georefMessage = "Calibrated manually with \(pairs.count) points (\u{00B1}\(max(1, Int(rms.rounded()))) m)"
        model.library.save(doc)
        onDone()
    }

    @MainActor
    private func cancel() {
        if pairs.isEmpty {
            dismiss()
        } else {
            showDiscard = true
        }
    }
}
