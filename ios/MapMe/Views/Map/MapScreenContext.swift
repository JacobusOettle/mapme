import CoreGraphics
import Foundation
import Observation
import SwiftUI

/// A sheet or full-screen cover a feature presents over the map screen (`MapScreenContext.sheet`,
/// `.cover`); the screen gives it the app model's environment.
struct MapPresentation: Identifiable {
    let id = UUID()
    let content: @MainActor () -> AnyView

    init(_ content: @escaping @MainActor () -> AnyView) {
        self.content = content
    }
}

/// A short notice at the top of the map screen ("Switched to Mhlabatini · more detailed here"),
/// gone after a few seconds, with an optional action ("Undo").
struct MapToast: Identifiable {
    let id = UUID()
    var text: String
    var symbol: String
    var actionTitle: String? = nil
    var action: (@MainActor () -> Void)? = nil
    /// Seconds on screen.
    var duration: Double = 5
}

/// A pin dropped by a long press: the card under it offers what to make of the spot.
struct DroppedPin: Equatable {
    var position: LatLon
}

/// The state of one map screen that features read and change from their own files
/// (docs/design/explorer.md, Map screen): the map shown (auto-switching changes it in place),
/// what is selected, the active tool, presentations and notices. Created by `MapScreen`; its
/// `overlays` once the screen appears.
@MainActor
@Observable
final class MapScreenContext {
    /// The map on screen: the one opened, until auto-switching or "Maps here" shows another.
    private(set) var mapID: UUID
    /// The shown map's tiles (replaced together with `mapID`, so the canvas never draws one map's
    /// tiles at another's size).
    private(set) var tileStore: TileStore?
    /// A geo item selected on the map, shown in its card.
    var selectedItem: GeoItemRef?
    /// Another map's point of interest selected on this one, shown in the POI card.
    var selectedForeignPOI: MapPOI?
    /// The long-press pin and its card.
    var droppedPin: DroppedPin?
    /// The tool taking the map's touches (route editing, freehand), nil for the plain map.
    var tool: (any MapTool)?
    /// Mirrors the screen's tracking mode (features read it: auto-switching only while following).
    var tracking: CanvasTrackingMode = .follow
    var sheet: MapPresentation?
    var cover: MapPresentation?
    var toast: MapToast?

    let controller: MapCanvasController
    /// The camera to carry to the next map (set by `switchMap`, applied by the canvas).
    @ObservationIgnored private(set) var pendingHandoff: CanvasCameraHandoff?
    /// The features' canvas content (made once the screen has the app model).
    @ObservationIgnored private(set) var overlays: CanvasOverlaySet?
    @ObservationIgnored private(set) weak var model: AppModel?

    init(mapID: UUID) {
        self.mapID = mapID
        controller = MapCanvasController()
    }

    /// Connects the screen to the app model and makes the features' overlays (once).
    func attach(_ model: AppModel) {
        guard self.model == nil else { return }
        self.model = model
        if let map = model.library.map(id: mapID) {
            tileStore = TileStore(map: map, library: model.library)
        }
        overlays = CanvasOverlaySet(model: model, context: self)
    }

    /// Shows another map in place, keeping the scale in metres, the direction of north on screen,
    /// the place in the middle and the tracking mode. `announce` shows a toast (with "Undo").
    func switchMap(to id: UUID, announce: String? = nil) {
        guard id != mapID, let library = model?.library, let map = library.map(id: id) else { return }
        let previous = mapID
        pendingHandoff = controller.cameraHandoff(to: id)
        selectedItem = nil
        selectedForeignPOI = nil
        droppedPin = nil
        tileStore = TileStore(map: map, library: library)
        mapID = id
        if let announce {
            toast = MapToast(text: announce, symbol: "map", actionTitle: "Undo", action: { [weak self] in
                self?.switchMap(to: previous)
            })
        }
    }

    func show(_ toast: MapToast) {
        self.toast = toast
    }

    func dismissToast(_ id: UUID) {
        if toast?.id == id { toast = nil }
    }

    /// Clears every selection (a tap on empty map).
    func clearSelection() {
        selectedItem = nil
        selectedForeignPOI = nil
        droppedPin = nil
    }
}
