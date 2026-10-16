import SwiftUI
import UIKit

// The selected waypoint's and route's cards on the map screen (docs/design/explorer.md, Routes:
// Waypoints, Follow; Profile).

/// A waypoint's marker as a SwiftUI view (the canvas's red disc and symbol).
struct WaypointGlyph: View {
    let symbol: WaypointSymbol
    var size: CGFloat = 40

    var body: some View {
        ZStack {
            Circle().fill(Color.white)
            Circle()
                .fill(Color(uiColor: RouteArt.waypointRed).gradient)
                .padding(size * 0.07)
            Image(systemName: symbol.systemImage)
                .font(.system(size: size * 0.4, weight: .semibold))
                .foregroundStyle(Color.white)
        }
        .frame(width: size, height: size)
        .shadow(color: Color.black.opacity(0.25), radius: 2, y: 1)
        .accessibilityHidden(true)
    }
}

/// The card's round close button.
struct RouteCardCloseButton: View {
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark")
                .font(.system(size: 12, weight: .bold))
                .foregroundStyle(.secondary)
                .frame(width: 30, height: 30)
                .background(Color.primary.opacity(0.1), in: Circle())
        }
        .buttonStyle(PressScaleButtonStyle())
        .accessibilityLabel("Close")
    }
}

/// A rounded action tile of a card (Route Here, Share, Follow…).
struct RouteCardActionLabel: View {
    let title: String
    let symbol: String
    var tint: Color = Theme.accent
    var prominent = false

    var body: some View {
        VStack(spacing: 4) {
            Image(systemName: symbol)
                .font(.system(size: 16, weight: .semibold))
            Text(title)
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .minimumScaleFactor(0.8)
        }
        .foregroundStyle(prominent ? Color.white : tint)
        .frame(maxWidth: .infinity, minHeight: 52)
        .background(prominent ? AnyShapeStyle(tint) : AnyShapeStyle(Color.primary.opacity(0.08)),
                    in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

/// Where a geo item came from, for a card's subtitle.
enum RouteItemOrigin {
    static func text(_ origin: GeoItemOrigin) -> String {
        switch origin {
        case .user: return "your waypoint"
        case .gpx(let fileName): return "from \(fileName)"
        case .migrated: return "your waypoint"
        }
    }
}

// MARK: - Waypoint

/// A selected waypoint: name, distance and direction, elevation; rename, symbol, route here,
/// delete, export.
@MainActor
struct WaypointCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model
    @State private var renaming = false
    @State private var nameText = ""
    @State private var confirmDelete = false

    var body: some View {
        if let waypoint = model.geoItems.waypoint(id: id) {
            content(waypoint)
        } else {
            Color.clear
                .frame(height: 0)
                .onAppear { context.selectedItem = nil }
        }
    }

    private func content(_ w: Waypoint) -> some View {
        let units = model.settings.units
        let here = model.location.location.map { LatLon($0.coordinate) }
        return VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                WaypointGlyph(symbol: w.symbol)
                VStack(alignment: .leading, spacing: 2) {
                    Text(w.name)
                        .font(.headline)
                        .lineLimit(1)
                        .accessibilityIdentifier("waypointCardName")
                    HStack(spacing: 6) {
                        if let here {
                            let d = Geodesy.distance(here, w.position)
                            Text(d < 25 ? "Here" : "\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(Geodesy.bearing(here, w.position)))")
                                .monospacedDigit()
                            Text("·")
                        }
                        if let e = w.elevationM {
                            Text(GeoFormat.elevation(e, units: units))
                                .monospacedDigit()
                            Text("·")
                        }
                        Text(RouteItemOrigin.text(w.origin))
                            .lineLimit(1)
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                RouteCardCloseButton {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { context.selectedItem = nil }
                }
            }
            if let comment = w.comment, !comment.isEmpty {
                Text(comment)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            HStack(spacing: 8) {
                Button {
                    RouteActions.routeHere(to: w.position, context: context, model: model, name: w.name)
                } label: {
                    RouteCardActionLabel(title: "Route Here", symbol: "point.topleft.down.to.point.bottomright.curvepath",
                                    tint: Theme.route)
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .disabled(map.georef == nil)
                .accessibilityIdentifier("waypointRouteHereButton")

                ShareLink(item: GPXExportFile(name: w.name, waypoints: [w]),
                          preview: SharePreview(w.name, image: Image(systemName: w.symbol.systemImage))) {
                    RouteCardActionLabel(title: "Share", symbol: "square.and.arrow.up")
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .accessibilityIdentifier("waypointShareButton")

                Menu {
                    Button {
                        nameText = w.name
                        renaming = true
                    } label: {
                        Label("Rename…", systemImage: "pencil")
                    }
                    .accessibilityIdentifier("waypointRenameButton")
                    Menu {
                        ForEach(WaypointSymbol.allCases) { s in
                            Button {
                                setSymbol(s, of: w)
                            } label: {
                                Label(s.label, systemImage: s == w.symbol ? "checkmark" : s.systemImage)
                            }
                        }
                    } label: {
                        Label("Symbol", systemImage: w.symbol.systemImage)
                    }
                    Divider()
                    Button(role: .destructive) {
                        confirmDelete = true
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                    .accessibilityIdentifier("waypointDeleteButton")
                } label: {
                    RouteCardActionLabel(title: "More", symbol: "ellipsis")
                }
                .accessibilityIdentifier("waypointMoreButton")
            }
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("waypointCard")
        .task(id: w.id) { await fillElevation(w) }
        .alert("Rename waypoint", isPresented: $renaming) {
            TextField("Name", text: $nameText)
            Button("Cancel", role: .cancel) {}
            Button("Save") { rename(w) }
        }
        .confirmationDialog("Delete \u{201C}\(w.name)\u{201D}?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Waypoint", role: .destructive) {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    context.selectedItem = nil
                    model.geoItems.deleteWaypoint(w.id)
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func rename(_ w: Waypoint) {
        let name = nameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, var current = model.geoItems.waypoint(id: w.id) else { return }
        current.name = name
        model.geoItems.save(current)
    }

    private func setSymbol(_ symbol: WaypointSymbol, of w: Waypoint) {
        guard var current = model.geoItems.waypoint(id: w.id), current.symbol != symbol else { return }
        current.symbol = symbol
        model.geoItems.save(current)
    }

    /// A waypoint without an elevation gets the terrain's (kept with it).
    private func fillElevation(_ w: Waypoint) async {
        guard w.elevationM == nil else { return }
        if let e = await WaypointActions.terrainElevation(at: w.position, terrain: model.terrain),
           var current = model.geoItems.waypoint(id: w.id), current.elevationM == nil {
            current.elevationM = e
            model.geoItems.save(current)
        }
    }
}

// MARK: - Route

/// A selected route: distance, climb, descent, walking time, the elevation profile; follow,
/// edit, export, delete.
@MainActor
struct RouteCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model
    @State private var data: RouteProfileData?
    @State private var loading = false
    @State private var renaming = false
    @State private var nameText = ""
    @State private var confirmDelete = false

    var body: some View {
        if let route = model.geoItems.route(id: id) {
            content(route)
        } else {
            Color.clear
                .frame(height: 0)
                .onAppear { context.selectedItem = nil }
        }
    }

    private func content(_ route: Route) -> some View {
        let units = model.settings.units
        let navigator = model.navigator
        let following = navigator.activeRouteID == route.id
        let pace = navigator.currentPace()
        // A card shown for another route a moment ago still holds that route's profile.
        let current = data.flatMap { $0.matches(route) ? $0 : nil }
        let profile = current?.profile
        let stats = profile.map { RoutePlanner.stats(for: route, profile: $0.profile) } ?? route.currentStats
        let distance = stats?.distanceM ?? 0
        let time: TimeInterval? = profile.flatMap { p in
            p.hasElevation ? PaceModel.movingTime(along: p.profile, factor: pace.factor) : nil
        } ?? (distance > 0 ? PaceModel.flatTime(meters: distance, factor: pace.factor) : nil)
        return VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                SymbolBadge(symbol: route.kind == .gpxTrack ? "point.bottomleft.forward.to.point.topright.scurvepath"
                                : "point.topleft.down.to.point.bottomright.curvepath",
                            color: route.displayColor.color, size: 40)
                VStack(alignment: .leading, spacing: 2) {
                    Text(route.name)
                        .font(.headline)
                        .lineLimit(1)
                        .accessibilityIdentifier("routeCardName")
                    Text(subtitle(route))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                RouteCardCloseButton {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { context.selectedItem = nil }
                }
            }
            HStack(alignment: .top, spacing: 8) {
                PanelStat(caption: "Distance", value: stats.map { GeoFormat.distance($0.distanceM, units: units) } ?? "—")
                PanelStat(caption: "Ascent", value: stats?.ascentM.map { "↑" + GeoFormat.elevation($0, units: units) } ?? "—")
                PanelStat(caption: "Descent", value: stats?.descentM.map { "↓" + GeoFormat.elevation($0, units: units) } ?? "—")
                PanelStat(caption: "Time", value: time.map { RouteFormat.shortDuration($0) } ?? "—")
            }
            .accessibilityIdentifier("routeCardStats")
            profileSection(route, data: current, following: following, units: units)
            actions(route, following: following)
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("routeCard")
        .task(id: route.geometryHash) { await computeProfile(route) }
        .onDisappear { model.navigator.screen.scrubPoint = nil }
        .alert("Rename route", isPresented: $renaming) {
            TextField("Name", text: $nameText)
            Button("Cancel", role: .cancel) {}
            Button("Save") { rename(route) }
        }
        .confirmationDialog("Delete \u{201C}\(route.name)\u{201D}?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Route", role: .destructive) {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    if following { navigator.stop() }
                    context.selectedItem = nil
                    model.geoItems.deleteRoute(route.id)
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func subtitle(_ route: Route) -> String {
        switch (route.kind, route.origin) {
        case (.planned, _): return "Planned route · \(route.anchors.count) points"
        case (.gpxRoute, .gpx(let file)): return "GPX route · \(file)"
        case (.gpxTrack, .gpx(let file)): return "GPX track · \(file)"
        case (.gpxRoute, _): return "GPX route"
        case (.gpxTrack, _): return "GPX track"
        }
    }

    @ViewBuilder
    private func profileSection(_ route: Route, data: RouteProfileData?, following: Bool, units: DistanceUnits) -> some View {
        if let data {
            let profile = data.profile
            ElevationProfileView(profile: profile.profile, source: profile.source, units: units,
                                 color: route.displayColor.color,
                                 progressM: following ? model.navigator.progress.map { data.profileDistance(alongLine: $0.distanceDoneM) } : nil,
                                 markers: data.markers,
                                 height: 96,
                                 onScrub: { meters in
                                     model.navigator.screen.scrubPoint = meters.flatMap { data.point(alongProfile: $0) }
                                 })
                .accessibilityIdentifier("routeProfile")
                .overlay(alignment: .topTrailing) {
                    if profile.hasElevation {
                        Button {
                            let routeID = route.id
                            context.cover = MapPresentation { AnyView(RouteProfileScreen(routeID: routeID)) }
                        } label: {
                            Image(systemName: "arrow.up.left.and.arrow.down.right")
                                .font(.system(size: 12, weight: .bold))
                                .foregroundStyle(.secondary)
                                .frame(width: 30, height: 30)
                                .background(Color.primary.opacity(0.08), in: Circle())
                        }
                        .buttonStyle(PressScaleButtonStyle())
                        .accessibilityLabel("Show profile full screen")
                        .accessibilityIdentifier("routeProfileExpandButton")
                    }
                }
        } else if loading {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Working out the climb…")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        }
    }

    private func actions(_ route: Route, following: Bool) -> some View {
        HStack(spacing: 8) {
            if following {
                Button {
                    model.navigator.stop()
                } label: {
                    RouteCardActionLabel(title: "Stop", symbol: "stop.fill", tint: Theme.bad, prominent: true)
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .accessibilityIdentifier("routeStopButton")
            } else {
                Button {
                    model.navigator.follow(route)
                } label: {
                    RouteCardActionLabel(title: "Follow", symbol: "location.north.line.fill", tint: route.displayColor.color,
                                    prominent: true)
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .accessibilityIdentifier("routeFollowButton")
            }
            if route.kind != .gpxTrack {
                Button {
                    RouteEditTool.start(context: context, model: model, route: route)
                } label: {
                    RouteCardActionLabel(title: "Edit", symbol: "pencil")
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .disabled(map.georef == nil)
                .accessibilityIdentifier("routeEditButton")
            }
            ShareLink(item: GPXExportFile(name: route.name, routes: [route]),
                      preview: SharePreview(route.name, image: Image(systemName: "point.topleft.down.to.point.bottomright.curvepath"))) {
                RouteCardActionLabel(title: "Share", symbol: "square.and.arrow.up")
            }
            .buttonStyle(PressScaleButtonStyle(scale: 0.95))
            .accessibilityIdentifier("routeShareButton")
            Menu {
                Button {
                    nameText = route.name
                    renaming = true
                } label: {
                    Label("Rename…", systemImage: "pencil")
                }
                Button {
                    reverse(route)
                } label: {
                    Label("Reverse", systemImage: "arrow.left.arrow.right")
                }
                Menu {
                    ForEach(RouteColor.allCases) { c in
                        Button {
                            setColor(c, of: route)
                        } label: {
                            Label(c.rawValue.capitalized, systemImage: c == route.displayColor ? "checkmark" : "circle.fill")
                        }
                    }
                } label: {
                    Label("Colour", systemImage: "paintpalette")
                }
                Divider()
                Button(role: .destructive) {
                    confirmDelete = true
                } label: {
                    Label("Delete", systemImage: "trash")
                }
                .accessibilityIdentifier("routeDeleteButton")
            } label: {
                RouteCardActionLabel(title: "More", symbol: "ellipsis")
            }
            .accessibilityIdentifier("routeMoreButton")
        }
    }

    // MARK: Actions

    private func computeProfile(_ route: Route) async {
        loading = true
        defer { loading = false }
        guard let computed = await RouteProfileData.make(for: route, waypoints: model.geoItems.waypoints,
                                                         terrain: model.terrain),
              !Task.isCancelled else { return }
        data = computed
        // Keep the statistics with the route (lists and the world view show them).
        guard var current = model.geoItems.route(id: route.id), current.geometryHash == route.geometryHash else { return }
        let stats = RoutePlanner.stats(for: current, profile: computed.profile.profile)
        if current.stats != stats {
            current.stats = stats
            model.geoItems.save(current)
        }
    }

    private func rename(_ route: Route) {
        let name = nameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, var current = model.geoItems.route(id: route.id) else { return }
        current.name = name
        current.updatedAt = Date()
        model.geoItems.save(current)
    }

    private func reverse(_ route: Route) {
        guard var current = model.geoItems.route(id: route.id) else { return }
        current.points.reverse()
        current.updatedAt = Date()
        current.stats = nil
        model.geoItems.save(current)
    }

    private func setColor(_ color: RouteColor, of route: Route) {
        guard var current = model.geoItems.route(id: route.id), current.displayColor != color else { return }
        current.color = color
        model.geoItems.save(current)
    }
}
