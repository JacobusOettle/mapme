import CoreLocation
import QuartzCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

// The routes feature on the map screen (docs/design/explorer.md, Routes): the side column's
// route button, the guidance and daylight banner, and the dropped pin's actions. The cards are in
// RouteCards.swift, the canvas overlays in RouteOverlays.swift, planning in RouteEditTool.swift.

/// Side column: plan a route, your routes, import GPX, follow. Also presents what other screens
/// ask the map screen for (a GPX import's summary, a selection made in a list).
@MainActor
struct RouteToolsButton: View {
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model
    @State private var importing = false

    var body: some View {
        let navigator = model.navigator
        let editing = context.tool is RouteEditTool
        let selectedRoute = context.selectedItem.flatMap { ref in ref.kind == .route ? model.geoItems.route(id: ref.id) : nil }
        let active = editing || navigator.activeRouteID != nil
        Menu {
            Button {
                RouteEditTool.start(context: context, model: model)
            } label: {
                Label("New Route", systemImage: "plus")
            }
            .disabled(map.georef == nil || editing)
            .accessibilityIdentifier("newRouteButton")
            Button {
                showList()
            } label: {
                Label("Routes…", systemImage: "list.bullet")
            }
            .accessibilityIdentifier("routesListButton")
            Button {
                importing = true
            } label: {
                Label("Import GPX…", systemImage: "square.and.arrow.down")
            }
            .accessibilityIdentifier("importGPXButton")
            if let route = selectedRoute, navigator.activeRouteID != route.id {
                Divider()
                Button {
                    navigator.follow(route)
                } label: {
                    Label("Follow \u{201C}\(route.name)\u{201D}", systemImage: "location.north.line.fill")
                }
                .accessibilityIdentifier("followRouteMenuButton")
            }
            if let followed = navigator.activeRoute {
                Divider()
                Button(role: .destructive) {
                    navigator.stop()
                } label: {
                    Label("Stop Following \u{201C}\(followed.name)\u{201D}", systemImage: "stop.fill")
                }
                .accessibilityIdentifier("stopFollowingMenuButton")
            }
        } label: {
            Image(systemName: "point.topleft.down.to.point.bottomright.curvepath")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(active ? Theme.route : Color.primary)
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .floatingChrome(Circle(), interactive: true)
        .opacity(map.georef == nil ? 0.55 : 1)
        .accessibilityLabel("Routes")
        .accessibilityIdentifier("routeToolsButton")
        .fileImporter(isPresented: $importing, allowedContentTypes: [.gpx]) { result in
            if case .success(let url) = result { model.gpx.importFile(at: url) }
        }
        .onAppear {
            presentImportSummary()
            applySelectionRequest()
        }
        .onChange(of: model.gpx.lastImport?.id) { presentImportSummary() }
        .onChange(of: model.navigator.screen.selectionRequest) { applySelectionRequest() }
        .onChange(of: context.mapID) { model.navigator.screen.scrubPoint = nil }
    }

    private func showList() {
        let ctx = context
        context.sheet = MapPresentation { AnyView(RoutesListSheet(context: ctx)) }
    }

    /// A GPX import's summary, once, on this screen: after any sheet that is closing has gone (the
    /// routes list's Import closes it as the file arrives).
    private func presentImportSummary() {
        guard model.gpx.lastImport != nil else { return }
        let context = self.context
        let model = self.model
        Task { @MainActor in
            await PresentationGate.settled()
            guard context.sheet == nil, context.cover == nil, let summary = model.gpx.takeSummaryToPresent() else { return }
            let id = summary.id
            context.sheet = MapPresentation { AnyView(GPXImportSummarySheet(summaryID: id)) }
        }
    }

    /// An item picked in Nearby, a list or a summary: selected here.
    private func applySelectionRequest() {
        guard let ref = model.navigator.screen.selectionRequest else { return }
        model.navigator.screen.selectionRequest = nil
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
            context.clearSelection()
            context.selectedItem = ref
        }
    }
}

/// Under the title: off-route warning, progress and ETA while following, the daylight left
/// ("2 h 10 min of daylight left; at your pace you reach Bannerman Hut at 17:40").
@MainActor
struct RouteGuidanceBanner: View {
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model

    var body: some View {
        let navigator = model.navigator
        let fix = model.location.location
        let now = RoutesClock.now()
        // The day's sun where you are (or, without a fix, along the route you follow).
        let place = fix.map { LatLon($0.coordinate) } ?? navigator.activeRoute?.points.first?.latLon
        let sun = place.map { SunTimes.compute(on: now, at: $0) }
        if let route = navigator.activeRoute {
            let progress = navigator.progress
            let forecast = sun.flatMap { s in
                DaylightForecast.make(now: now, sun: s, eta: progress?.destinationETA ?? progress?.eta,
                                      destination: progress?.destinationName ?? RouteNavigator.destinationName(of: route))
            }
            FollowingBanner(route: route, progress: progress, isOffRoute: navigator.isOffRoute,
                            hasArrived: navigator.hasArrived, forecast: forecast, units: model.settings.units,
                            controller: context.controller,
                            onStop: { navigator.stop() },
                            onDaylight: { showDaylight() })
                .transition(.move(edge: .top).combined(with: .opacity))
        } else if fix != nil, let sun,
                  let forecast = DaylightForecast.make(now: now, sun: sun, eta: nil, destination: nil) {
            DaylightChip(forecast: forecast) { showDaylight() }
                .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    private func showDaylight() {
        context.sheet = MapPresentation { AnyView(DaylightSheet()) }
    }
}

/// The banner while following: off route (red), arrived (green) or progress, then the daylight.
@MainActor
private struct FollowingBanner: View {
    let route: Route
    let progress: RouteProgress?
    let isOffRoute: Bool
    let hasArrived: Bool
    let forecast: DaylightForecast?
    let units: DistanceUnits
    /// The canvas's readouts: north's direction on screen points the way back to the route (read
    /// only while off route: it changes as the map turns).
    let controller: MapCanvasController
    var onStop: () -> Void
    var onDaylight: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                header
                Spacer(minLength: 4)
                Button(action: onStop) {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(.secondary)
                        .frame(width: 26, height: 26)
                        .background(Color.primary.opacity(0.1), in: Circle())
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(PressScaleButtonStyle())
                .padding(-9)
                .accessibilityLabel("Stop following")
                .accessibilityIdentifier("routeStopFollowingButton")
            }
            if let forecast {
                Button(action: onDaylight) {
                    DaylightLine(forecast: forecast)
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.97))
                .accessibilityIdentifier("daylightChip")
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background {
            if isOffRoute {
                RoundedRectangle(cornerRadius: 20, style: .continuous).fill(Theme.bad.opacity(0.22))
            }
        }
        .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(isOffRoute ? "offRouteBanner" : "routeGuidanceBanner")
    }

    @ViewBuilder
    private var header: some View {
        if isOffRoute, let p = progress {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(Theme.bad)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Off route · \(GeoFormat.distance(p.distanceFromRouteM, units: units))")
                        .font(.subheadline.weight(.bold))
                    if let bearing = p.bearingToRouteDegrees {
                        HStack(spacing: 4) {
                            Text("Back")
                            Image(systemName: "arrow.up")
                                .font(.caption.weight(.heavy))
                                .rotationEffect(.degrees(controller.northDegrees + bearing))
                            Text(Geodesy.compassPoint(bearing))
                        }
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.secondary)
                    }
                }
            }
        } else if hasArrived {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(Theme.good)
                Text("Arrived at \(RouteNavigator.destinationName(of: route))")
                    .font(.subheadline.weight(.bold))
                    .lineLimit(2)
            }
            .accessibilityIdentifier("routeArrivedBanner")
        } else if let p = progress {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Image(systemName: "location.north.line.fill")
                        .foregroundStyle(route.displayColor.color)
                    Text("\(GeoFormat.distance(p.distanceRemainingM, units: units)) to go")
                        .font(.subheadline.weight(.bold))
                        .monospacedDigit()
                    if let up = p.ascentRemainingM, up >= 1 {
                        Text("↑\(GeoFormat.elevation(up, units: units))")
                            .font(.subheadline.weight(.semibold))
                            .monospacedDigit()
                    }
                    if let t = p.remainingTime {
                        Text("· \(RouteFormat.duration(t))")
                            .font(.subheadline.weight(.semibold))
                            .monospacedDigit()
                    }
                }
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                if let next = p.nextName, let d = p.nextDistanceM {
                    Text("\(route.name) · next: \(next) in \(GeoFormat.distance(d, units: units))")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                } else {
                    Text(route.name)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        } else {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Following \(route.name) · waiting for GPS")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
            }
        }
    }
}

/// The daylight sentence, coloured by how tight it is.
private struct DaylightLine: View {
    let forecast: DaylightForecast

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: forecast.level == .normal ? "sun.horizon.fill" : "exclamationmark.circle.fill")
                .foregroundStyle(DaylightStyle.color(forecast.level))
            Text(forecast.text)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(forecast.level == .normal ? Color.primary : DaylightStyle.color(forecast.level))
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// Without a route: "1 h 40 min of daylight left · sunset 18:02" in the last two hours of the day.
@MainActor
private struct DaylightChip: View {
    let forecast: DaylightForecast
    var onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            DaylightLine(forecast: forecast)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.97))
        .accessibilityIdentifier("daylightChip")
    }
}

enum DaylightStyle {
    static func color(_ level: DaylightForecast.Level) -> Color {
        switch level {
        case .normal: return Theme.warn.opacity(0.9)
        case .amber: return Color(red: 1.0, green: 0.62, blue: 0.04)
        case .red: return Theme.bad
        }
    }
}

/// The dropped pin's waypoint action.
@MainActor
enum WaypointActions {
    /// Saves a waypoint at `position` and selects it (its card fills in the terrain's elevation).
    static func saveWaypoint(at position: LatLon, context: MapScreenContext, model: AppModel) {
        let n = model.geoItems.waypoints.count + 1
        let waypoint = Waypoint(name: "Waypoint \(n)", position: position)
        model.geoItems.save(waypoint)
        context.droppedPin = nil
        context.selectedItem = GeoItemRef(kind: .waypoint, id: waypoint.id)
    }

    /// The terrain's elevation at a point (nil offline without terrain there).
    nonisolated static func terrainElevation(at p: LatLon, terrain: TerrainProvider) async -> Double? {
        let request = ElevationsRequest(points: [p])
        let answer = await terrain.resolve { store in store.elevations(request) }
        guard let value = answer?.value, let first = value.elevationsM.first else { return nil }
        return first
    }
}

/// The dropped pin's route action.
@MainActor
enum RouteActions {
    /// "From you" only when you are this close to where the route goes (a straight line from
    /// another valley helps nobody).
    static let fromYouWithinM = 30_000.0

    /// Starts planning a route from you (or the route being planned) to `position`.
    static func routeHere(to position: LatLon, context: MapScreenContext, model: AppModel) {
        routeHere(to: position, context: context, model: model, name: nil)
    }

    /// `routeHere`, the new end named (a waypoint's name).
    static func routeHere(to position: LatLon, context: MapScreenContext, model: AppModel, name: String?) {
        context.droppedPin = nil
        if let tool = context.tool as? RouteEditTool {
            tool.appendAnchor(position, name: name)
            return
        }
        var points: [RoutePoint] = []
        if let fix = model.location.location, fix.horizontalAccuracy >= 0,
           Geodesy.distance(LatLon(fix.coordinate), position) <= fromYouWithinM {
            points.append(RoutePoint(LatLon(fix.coordinate), isAnchor: true))
        }
        points.append(RoutePoint(position, isAnchor: true, name: name))
        RouteEditTool.start(context: context, model: model, points: points)
    }
}
