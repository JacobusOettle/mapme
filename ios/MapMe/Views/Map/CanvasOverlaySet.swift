import Foundation

/// The features' content on one map screen's canvas, bottom to top within each level. Each type
/// lives in its feature's files and reads what it draws from the app model and the screen's
/// context itself; add a feature's overlay here (one line) and nowhere else.
@MainActor
final class CanvasOverlaySet {
    /// Screen-space overlays.
    let overlays: [any CanvasOverlay]
    /// Views in map-pixel space.
    let mapSpaceLayers: [any CanvasMapSpaceLayer]

    init(model: AppModel, context: MapScreenContext) {
        overlays = [
            MapEdgeOverlay(model: model, context: context),        // maps: the sheet's edge
            RouteOverlay(model: model, context: context),          // routes: planned and imported
            WaypointOverlay(model: model, context: context),       // routes: waypoints
            OtherMapsPOIOverlay(model: model, context: context),   // maps: your other maps' places
            NoteOverlay(model: model, context: context),           // notes: notes and photos
        ]
        mapSpaceLayers = [
            OffMapTerrainLayer(model: model, context: context),    // maps: terrain beyond the sheet
        ]
    }
}
