import QuartzCore
import SwiftUI
import UIKit

// The routes feature on the map screen (docs/design/explorer.md, Routes). Stubs: the routes
// feature fills them in (and may split them into more files).

/// Side column: plan a route, your routes, import GPX, follow. Empty until filled in.
@MainActor
struct RouteToolsButton: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// Under the title: off-route warning, progress and ETA while following, the daylight left
/// ("2 h 10 min of daylight left; at your pace you reach Bannerman Hut at 17:40").
@MainActor
struct RouteGuidanceBanner: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// A selected waypoint: name, distance and direction, elevation; rename, symbol, route here,
/// delete, export.
@MainActor
struct WaypointCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// A selected route: distance, climb, descent, walking time, the elevation profile; follow,
/// edit, export, delete.
@MainActor
struct RouteCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// Planned and imported routes on the canvas (under the trail), the followed one highlighted.
@MainActor
final class RouteOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.routes
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
    }

    func mapDidChange(_ context: CanvasMapContext) {}

    func render(_ frame: CanvasFrame) {}
}

/// Waypoints on the canvas (above the map's own POIs).
@MainActor
final class WaypointOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.markers
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
    }

    func mapDidChange(_ context: CanvasMapContext) {}

    func render(_ frame: CanvasFrame) {}
}

/// The dropped pin's waypoint action.
@MainActor
enum WaypointActions {
    /// Saves a waypoint at `position` and selects it.
    static func saveWaypoint(at position: LatLon, context: MapScreenContext, model: AppModel) {
        let n = model.geoItems.waypoints.count + 1
        let waypoint = Waypoint(name: "Waypoint \(n)", position: position)
        model.geoItems.save(waypoint)
        context.droppedPin = nil
        context.selectedItem = GeoItemRef(kind: .waypoint, id: waypoint.id)
    }
}

/// The dropped pin's route action.
@MainActor
enum RouteActions {
    /// Starts planning a route from you (or the route being planned) to `position`.
    static func routeHere(to position: LatLon, context: MapScreenContext, model: AppModel) {}
}

/// Settings → Routes: walking pace (learned or set), off-route alerts, daylight warnings.
@MainActor
struct RoutesSettingsSection: View {
    var body: some View {
        EmptyView()
    }
}
