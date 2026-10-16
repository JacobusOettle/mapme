import CoreGraphics
import Foundation
import Observation
import QuartzCore
import SwiftUI
import UIKit

// Extension points of the map canvas (docs/design/explorer.md, Map screen): features draw their
// content (routes, waypoints, notes, other maps' POIs, the sheet's edge, the terrain beyond it)
// through these protocols from their own files; `CanvasOverlaySet` lists them, the canvas places
// and renders them. Everything here runs on the main thread except where noted.

/// What an overlay needs to place geo content on the current map.
struct CanvasMapContext {
    let map: MapDocument
    let georef: Georeference?
    /// Coordinates → map pixels (homography and warp), worked out once per map.
    let projector: Georeference.Projector?
    /// Map pixels shown beyond each edge of the sheet (the terrain continuation; 0 = none).
    let continuationMargin: CGFloat
    let darkMap: Bool
    let units: DistanceUnits
    /// Asks the canvas for a frame (after the overlay's content changed while nothing moves).
    let requestFrame: @MainActor () -> Void

    /// The sheet in map pixels.
    var sheetRect: CGRect { CGRect(origin: .zero, size: map.pixelSize) }

    /// Where overlays draw: the sheet and its continuation.
    var drawableRect: CGRect { sheetRect.insetBy(dx: -continuationMargin, dy: -continuationMargin) }

    /// The map pixel of a coordinate (nil when the map is not located or it does not project).
    func pixel(for p: LatLon) -> CGPoint? {
        guard let projector else { return nil }
        let px = projector.pixel(for: p)
        return px.x.isFinite && px.y.isFinite ? px : nil
    }

    /// The coordinate of a map pixel.
    func latLon(forPixel p: CGPoint) -> LatLon? {
        georef?.latLon(forPixel: p)
    }
}

/// One frame of the canvas camera.
struct CanvasFrame {
    let camera: CanvasCamera
    let viewSize: CGSize
    /// Map pixel → view point.
    let transform: CGAffineTransform
    /// The part of the view not under floating controls.
    let contentRect: CGRect
    /// Ground metres per view point at the view's centre (0 when the map is not located).
    let metersPerPoint: Double
    let timestamp: CFTimeInterval

    func viewPoint(forMap p: CGPoint) -> CGPoint { p.applying(transform) }
}

/// Where a feature's layer sits among the canvas's own (the puck always stays on top).
enum CanvasOverlayLevel: Int, Comparable, CaseIterable {
    /// Under your walked trail: routes, the sheet's edge, coverage outlines.
    case routes = 0
    /// Above the map's own POI markers, under the puck: waypoints, notes and photos, other maps'
    /// POIs.
    case markers = 1
    /// Above everything: a handle being dragged, the dropped pin.
    case top = 2

    static func < (a: CanvasOverlayLevel, b: CanvasOverlayLevel) -> Bool { a.rawValue < b.rawValue }
}

/// A tap that landed on a feature's content.
struct CanvasHit: Equatable {
    /// A geo item (waypoint, route, note).
    var item: GeoItemRef?
    /// Or a point of interest of another map (shown in the POI card).
    var poi: MapPOI?
    /// Distance from the touch (points): the nearest hit wins.
    var distance: CGFloat
}

/// A feature's screen-space overlay on the map canvas (routes, waypoints, notes, other maps'
/// POIs, the sheet's edge). `render` runs every frame while the camera moves: keep it free of
/// allocations on the common path, with implicit animations off (the canvas wraps it in a
/// transaction). Observe the data you draw yourself (`ObservationLoop`) and call
/// `context.requestFrame()` after it changes.
@MainActor
protocol CanvasOverlay: AnyObject {
    var level: CanvasOverlayLevel { get }
    /// Added by the canvas at `level` (in view coordinates; it is never resized).
    var layer: CALayer { get }
    /// A new map, georeference, appearance, units or continuation: reproject.
    func mapDidChange(_ context: CanvasMapContext)
    func render(_ frame: CanvasFrame)
    /// The content under a view point (within finger reach, ~22 pt), if any.
    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit?
    /// Advances an animation; true while it needs more frames.
    func advance(_ now: CFTimeInterval) -> Bool
}

@MainActor
extension CanvasOverlay {
    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit? { nil }
    func advance(_ now: CFTimeInterval) -> Bool { false }
}

/// A feature's view in map-pixel space, moved with the map raster (center + transform like the
/// tiled view): the terrain drawn beyond the sheet's edge. Its bounds may extend beyond the sheet
/// (negative origin); the canvas puts the bounds' centre where that map pixel is on screen.
@MainActor
protocol CanvasMapSpaceLayer: AnyObject {
    var view: UIView { get }
    /// True: under the sheet (the sheet and its tiles cover it); false: above the map raster.
    var isBelowSheet: Bool { get }
    func mapDidChange(_ context: CanvasMapContext)
}

/// A tool that takes the map's touches while it is active: route editing, freehand tracing,
/// placing a note. Set `MapScreenContext.tool`; the bar replaces the stats panel.
@MainActor
protocol MapTool: AnyObject {
    /// Shown at the bottom instead of the stats panel (route editing: stats, Undo, Done).
    func makeBar() -> AnyView
    /// A tap on the map; true when the tool used it (no selection change then).
    func tap(at pixel: CGPoint, viewPoint: CGPoint) -> Bool
    func longPress(at pixel: CGPoint, viewPoint: CGPoint) -> Bool
    /// A one-finger drag starting at `pixel`; true to take it (the map does not pan): moving a
    /// route point, tracing a trail freehand.
    func beginDrag(at pixel: CGPoint, viewPoint: CGPoint) -> Bool
    func drag(to pixel: CGPoint, viewPoint: CGPoint)
    func endDrag(cancelled: Bool)
}

@MainActor
extension MapTool {
    func tap(at pixel: CGPoint, viewPoint: CGPoint) -> Bool { false }
    func longPress(at pixel: CGPoint, viewPoint: CGPoint) -> Bool { false }
    func beginDrag(at pixel: CGPoint, viewPoint: CGPoint) -> Bool { false }
    func drag(to pixel: CGPoint, viewPoint: CGPoint) {}
    func endDrag(cancelled: Bool) {}
}

/// The camera as seen on the ground, carried from one map to the next (auto-switching, "Maps
/// here"): the scale in metres, the direction of north on screen and the place in the middle.
struct CanvasCameraHandoff: Equatable {
    /// The map it is for (applied once, when that map appears).
    var toMapID: UUID
    /// Ground metres per view point.
    var metersPerPoint: Double
    /// Screen direction of true north, radians clockwise from up.
    var northOnScreen: Double
    /// The coordinate at the view's centre.
    var center: LatLon
}

/// Runs `body` now, and again on the main actor after any `@Observable` property it read
/// changes: UIKit overlays redraw model data (geo items, other maps, settings) this way, without
/// SwiftUI. `cancel()` (or releasing the loop) stops it.
@MainActor
final class ObservationLoop {
    private var body: (@MainActor () -> Void)?

    init(_ body: @escaping @MainActor () -> Void) {
        self.body = body
        observe()
    }

    func cancel() {
        body = nil
    }

    private func observe() {
        guard let body else { return }
        withObservationTracking {
            body()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observe()
            }
        }
    }
}
