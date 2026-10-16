import SwiftUI
import UniformTypeIdentifiers

// Sheets of the routes feature: the GPX import's summary, the list of routes and waypoints, and
// the day's daylight with the ETA's parts (docs/design/explorer.md, GPX; Daylight banner).

// MARK: - GPX import summary

/// "2 routes and 12 waypoints from Bannerman Pass.gpx", with Show on Map; or why the file could
/// not be read; or, for a file imported before, Replace / Keep Both.
@MainActor
struct GPXImportSummarySheet: View {
    let summaryID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let summary = model.gpx.lastImport.flatMap { $0.id == summaryID ? $0 : nil }
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let summary {
                        content(summary)
                    } else {
                        Text("Nothing to import.")
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .navigationTitle("GPX Import")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("gpxSummaryDoneButton")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .accessibilityIdentifier("gpxImportSummary")
        .onDisappear { model.gpx.dismissSummary(summaryID) }
    }

    @ViewBuilder
    private func content(_ summary: GPXImportSummary) -> some View {
        let units = model.settings.units
        HStack(alignment: .top, spacing: 12) {
            SymbolBadge(symbol: summary.error != nil ? "exclamationmark.triangle.fill" : "point.topleft.down.to.point.bottomright.curvepath",
                        color: summary.error != nil ? Theme.warn : Theme.importedRoute, size: 44)
            VStack(alignment: .leading, spacing: 4) {
                Text(summary.headline)
                    .font(.headline)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("gpxSummaryHeadline")
                if summary.isLoading {
                    ProgressView()
                        .controlSize(.small)
                }
            }
        }
        if let date = summary.duplicateImportedAt {
            VStack(alignment: .leading, spacing: 10) {
                Text("You imported this file on \(date.formatted(date: .abbreviated, time: .shortened)). Replace what it added then, or keep both?")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    Button("Replace") { model.gpx.resolveDuplicate(replace: true) }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.importedRoute)
                        .accessibilityIdentifier("gpxReplaceButton")
                    Button("Keep Both") { model.gpx.resolveDuplicate(replace: false) }
                        .buttonStyle(.bordered)
                        .accessibilityIdentifier("gpxKeepBothButton")
                }
                .font(.body.weight(.semibold))
            }
        } else if summary.error == nil, !summary.isLoading {
            let routes = summary.routeIDs.compactMap { model.geoItems.route(id: $0) }
            let waypoints = summary.waypointIDs.compactMap { model.geoItems.waypoint(id: $0) }
            Button {
                showOnMap(routes: routes, waypoints: waypoints)
            } label: {
                Label("Show on Map", systemImage: "map")
                    .font(.body.weight(.semibold))
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.importedRoute)
            .accessibilityIdentifier("gpxShowOnMapButton")
            VStack(alignment: .leading, spacing: 0) {
                ForEach(routes) { route in
                    HStack(spacing: 10) {
                        Circle().fill(route.displayColor.color).frame(width: 10, height: 10)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(route.name).font(.body.weight(.semibold)).lineLimit(1)
                            Text(RoutesListSheet.routeDetail(route, units: units))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 8)
                }
                ForEach(waypoints) { w in
                    HStack(spacing: 10) {
                        WaypointGlyph(symbol: w.symbol, size: 26)
                        Text(w.name).font(.body).lineLimit(1)
                        Spacer(minLength: 0)
                        if let e = w.elevationM {
                            Text(GeoFormat.elevation(e, units: units))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .monospacedDigit()
                        }
                    }
                    .padding(.vertical, 6)
                }
            }
        }
    }

    /// The most detailed located map covering most of the items (else the world view), with the
    /// first route (else waypoint) selected.
    private func showOnMap(routes: [Route], waypoints: [Waypoint]) {
        var positions = waypoints.map(\.position)
        for route in routes {
            // A long track: a few hundred of its points say where it is.
            let step = max(1, route.points.count / 300)
            positions += stride(from: 0, to: route.points.count, by: step).map { route.points[$0].latLon }
        }
        let best = GPXMapChooser.bestMap(for: positions, maps: model.library.maps)
        if let id = routes.first?.id {
            model.navigator.screen.selectionRequest = GeoItemRef(kind: .route, id: id)
        } else if let id = waypoints.first?.id {
            model.navigator.screen.selectionRequest = GeoItemRef(kind: .waypoint, id: id)
        }
        dismiss()
        if let best {
            model.openMap(best)
        } else {
            model.openWorld()
        }
    }
}

/// Picks the map to show imported items on.
enum GPXMapChooser {
    /// The located map containing the most of `positions`; among equals the most detailed (fewest
    /// metres per pixel at its centre). Nil when no map holds any.
    static func bestMap(for positions: [LatLon], maps: [MapDocument]) -> UUID? {
        var best: (id: UUID, count: Int, mpp: Double)?
        for map in maps {
            guard let g = map.georef else { continue }
            let count = positions.reduce(0) { $0 + (map.contains($1) ? 1 : 0) }
            guard count > 0 else { continue }
            let mid = CGPoint(x: Double(map.pixelWidth) / 2, y: Double(map.pixelHeight) / 2)
            let mpp = g.metersPerPixel(atPixel: mid)
            if let b = best, count < b.count || (count == b.count && mpp >= b.mpp) { continue }
            best = (id: map.id, count: count, mpp: mpp.isFinite ? mpp : .infinity)
        }
        return best?.id
    }
}

extension View {
    /// Presents a GPX import's summary over this screen (the library adds it; the map screen
    /// presents its own through its context).
    func gpxImportSummaryPresenter(_ model: AppModel) -> some View {
        modifier(GPXImportPresenter(model: model))
    }
}

@MainActor
private struct GPXImportPresenter: ViewModifier {
    let model: AppModel
    @State private var shown: GPXImportSummary?

    func body(content: Content) -> some View {
        content
            .onAppear { present() }
            .onChange(of: model.gpx.lastImport?.id) { present() }
            .sheet(item: $shown) { summary in
                GPXImportSummarySheet(summaryID: summary.id)
                    .environment(model)
            }
    }

    private func present() {
        guard shown == nil, model.gpx.lastImport != nil else { return }
        let model = self.model
        let shown = $shown
        Task { @MainActor in
            await PresentationGate.settled()
            // A map screen in front presents it itself (through its context, with its map).
            if case .map? = model.path.last { return }
            guard shown.wrappedValue == nil, let summary = model.gpx.takeSummaryToPresent() else { return }
            shown.wrappedValue = summary
        }
    }
}

// MARK: - Routes and waypoints

/// Your routes and waypoints (Routes… in the route menu): select one on the map, delete, import,
/// export all waypoints or everything on this map as GPX.
@MainActor
struct RoutesListSheet: View {
    let context: MapScreenContext
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var importing = false

    var body: some View {
        let units = model.settings.units
        let here = model.location.location.map { LatLon($0.coordinate) }
        let routes = model.geoItems.routes
        let waypoints = sortedWaypoints(here: here)
        NavigationStack {
            List {
                if routes.isEmpty && waypoints.isEmpty {
                    Section {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("No routes or waypoints yet")
                                .font(.headline)
                            Text("Plan a route with New Route, long-press the map for a waypoint, or import a GPX file from Gaia GPS, AllTrails or a friend.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 6)
                    }
                }
                if !routes.isEmpty {
                    Section("Routes") {
                        ForEach(routes) { route in
                            Button {
                                select(GeoItemRef(kind: .route, id: route.id))
                            } label: {
                                routeRow(route, units: units)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("routesListRoute")
                        }
                        .onDelete { offsets in
                            for i in offsets where i < routes.count {
                                if model.navigator.activeRouteID == routes[i].id { model.navigator.stop() }
                                model.geoItems.deleteRoute(routes[i].id)
                            }
                        }
                    }
                }
                if !waypoints.isEmpty {
                    Section("Waypoints") {
                        ForEach(waypoints) { w in
                            Button {
                                select(GeoItemRef(kind: .waypoint, id: w.id))
                            } label: {
                                waypointRow(w, here: here, units: units)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("routesListWaypoint")
                        }
                        .onDelete { offsets in
                            for i in offsets where i < waypoints.count { model.geoItems.deleteWaypoint(waypoints[i].id) }
                        }
                    }
                }
            }
            .navigationTitle("Routes & Waypoints")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        Button {
                            importing = true
                        } label: {
                            Label("Import GPX…", systemImage: "square.and.arrow.down")
                        }
                        if !model.geoItems.waypoints.isEmpty {
                            ShareLink(item: GPXExportFile(name: "MapMe waypoints", waypoints: model.geoItems.waypoints),
                                      preview: SharePreview("MapMe waypoints", image: Image(systemName: "flag.fill"))) {
                                Label("Export All Waypoints", systemImage: "square.and.arrow.up")
                            }
                        }
                        if let everything = everythingOnThisMap() {
                            ShareLink(item: everything,
                                      preview: SharePreview(everything.name, image: Image(systemName: "map"))) {
                                Label("Export Everything on This Map", systemImage: "square.and.arrow.up.on.square")
                            }
                        }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .accessibilityLabel("Import and export")
                    .accessibilityIdentifier("routesListMenu")
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.gpx]) { result in
                if case .success(let url) = result {
                    dismiss()
                    model.gpx.importFile(at: url)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .accessibilityIdentifier("routesListSheet")
    }

    private func sortedWaypoints(here: LatLon?) -> [Waypoint] {
        let all = model.geoItems.waypoints
        guard let here else { return all.sorted { $0.createdAt > $1.createdAt } }
        return all.sorted { Geodesy.distance(here, $0.position) < Geodesy.distance(here, $1.position) }
    }

    private func routeRow(_ route: Route, units: DistanceUnits) -> some View {
        let following = model.navigator.activeRouteID == route.id
        return HStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 3, style: .continuous)
                .fill(route.displayColor.color)
                .frame(width: 6, height: 34)
            VStack(alignment: .leading, spacing: 2) {
                Text(route.name)
                    .font(.body.weight(.semibold))
                    .lineLimit(1)
                Text(Self.routeDetail(route, units: units))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if following {
                Text("Following")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(route.displayColor.color)
            }
        }
        .contentShape(Rectangle())
    }

    private func waypointRow(_ w: Waypoint, here: LatLon?, units: DistanceUnits) -> some View {
        HStack(spacing: 12) {
            WaypointGlyph(symbol: w.symbol, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(w.name)
                    .font(.body.weight(.semibold))
                    .lineLimit(1)
                Text(RouteItemOrigin.text(w.origin))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if let here {
                Text(GeoFormat.distance(Geodesy.distance(here, w.position), units: units))
                    .font(.callout.weight(.semibold))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
        }
        .contentShape(Rectangle())
    }

    /// "Planned · 4.2 km · ↑ 320 m", "GPX track · 12.8 km".
    static func routeDetail(_ route: Route, units: DistanceUnits) -> String {
        let kind: String
        switch route.kind {
        case .planned: kind = "Planned"
        case .gpxRoute: kind = "GPX route"
        case .gpxTrack: kind = "GPX track"
        }
        let distance = route.currentStats?.distanceM ?? RouteGeometry.length(route.latLons)
        var parts = [kind, GeoFormat.distance(distance, units: units)]
        if let up = route.currentStats?.ascentM { parts.append("↑ " + GeoFormat.elevation(up, units: units)) }
        return parts.joined(separator: " · ")
    }

    private func select(_ ref: GeoItemRef) {
        context.clearSelection()
        context.selectedItem = ref
        dismiss()
    }

    /// Routes, waypoints and notes on the map shown (its corners, with a 2 km margin).
    private func everythingOnThisMap() -> GPXExportFile? {
        guard let map = model.library.map(id: context.mapID), let g = map.georef else { return nil }
        let w = CGFloat(map.pixelWidth), h = CGFloat(map.pixelHeight)
        let corners = [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)]
            .map { g.latLon(forPixel: $0) }
        guard let bounds = GeoBounds.enclosing(corners)?.expanded(byMeters: 2000) else { return nil }
        let store = model.geoItems
        let file = GPXExportFile(name: map.name, waypoints: store.waypoints(in: bounds),
                                 routes: store.routes(touching: bounds), notes: store.notes(in: bounds))
        guard !file.waypoints.isEmpty || !file.routes.isEmpty || !file.notes.isEmpty else { return nil }
        return file
    }
}

// MARK: - Daylight

/// The day's sun where you are, and while following the ETA's parts (tap on the daylight banner).
@MainActor
struct DaylightSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let now = RoutesClock.now()
        let navigator = model.navigator
        let place = model.location.location.map { LatLon($0.coordinate) } ?? navigator.activeRoute?.points.first?.latLon
        let sun = place.map { SunTimes.compute(on: now, at: $0) }
        let units = model.settings.units
        let pace = navigator.currentPace(now: now)
        NavigationStack {
            List {
                Section {
                    if let sun {
                        if sun.isPolarDay {
                            Text("The sun doesn't set today.")
                        } else if sun.isPolarNight {
                            Text("The sun doesn't rise today.")
                        }
                        row("Sunrise", sun.sunrise.map { Self.time($0) } ?? "—", symbol: "sunrise.fill")
                        row("Sunset", sun.sunset.map { Self.time($0) } ?? "—", symbol: "sunset.fill")
                        row("Dark (civil dusk)", sun.civilDusk.map { Self.time($0) } ?? "—", symbol: "moon.stars.fill")
                        row("Daylight left", sun.daylightLeft(at: now).map { RouteFormat.duration($0) } ?? "—",
                            symbol: "sun.max.fill")
                            .accessibilityIdentifier("daylightLeftRow")
                    } else {
                        Text("Waiting for your position.")
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Text("Today")
                } footer: {
                    Text("Computed on your iPhone for where you are (NOAA's solar calculator). After civil dusk it is too dark to walk without a light.")
                }
                if let route = navigator.activeRoute, let p = navigator.progress {
                    Section {
                        row("Destination", p.destinationName ?? RouteNavigator.destinationName(of: route), symbol: "flag.checkered")
                        row("Distance left", GeoFormat.distance(p.distanceRemainingM, units: units), symbol: "arrow.left.and.right")
                        if let up = p.ascentRemainingM {
                            row("Climb left", "↑ \(GeoFormat.elevation(up, units: units))  ↓ \(GeoFormat.elevation(p.descentRemainingM ?? 0, units: units))",
                                symbol: "arrow.up.right")
                        }
                        if let t = p.remainingTime {
                            row("Moving time left", RouteFormat.duration(t), symbol: "figure.hiking")
                        }
                        if let eta = p.destinationETA ?? p.eta {
                            row("You arrive", Self.time(eta), symbol: "clock.fill")
                        }
                    } header: {
                        Text(route.name)
                    } footer: {
                        Text(paceFootnote(pace: pace, flat: p.timeIsFlatEstimate))
                    }
                } else {
                    Section {
                        row("Your pace", String(format: "%.2f× Tobler", pace.factor), symbol: "figure.hiking")
                    } footer: {
                        Text(paceFootnote(pace: pace, flat: false))
                    }
                }
            }
            .navigationTitle("Daylight")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .accessibilityIdentifier("daylightSheet")
    }

    private func row(_ title: String, _ value: String, symbol: String) -> some View {
        HStack {
            Label(title, systemImage: symbol)
            Spacer(minLength: 8)
            Text(value)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }

    private func paceFootnote(pace: PaceEstimate, flat: Bool) -> String {
        var text = "At your pace: \(pace.description). Moving time only: breaks are not included."
        if flat { text += " Without terrain for this route the time assumes flat ground." }
        return text
    }

    private static func time(_ date: Date) -> String {
        date.formatted(date: .omitted, time: .shortened)
    }
}
