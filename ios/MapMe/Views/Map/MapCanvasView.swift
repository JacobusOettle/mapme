import CoreLocation
import SwiftUI
import UIKit

/// A marker drawn at a map pixel (used by calibration for numbered control points).
struct CanvasMarker: Identifiable, Hashable {
    var id: String
    /// Base-image pixel.
    var pixel: CGPoint
    var label: String
    var color: UIColor = .systemYellow

    // Spelled out because CGPoint is not Hashable.
    static func == (a: CanvasMarker, b: CanvasMarker) -> Bool {
        a.id == b.id && a.pixel == b.pixel && a.label == b.label && a.color == b.color
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
        hasher.combine(pixel.x)
        hasher.combine(pixel.y)
        hasher.combine(label)
    }
}

/// SwiftUI wrapper around the high-performance UIKit map canvas
/// (CATiledLayer pyramid, pan/pinch/rotate, tracking modes, overlays).
///
/// Two ways to drive the camera: the live map passes `tracking` (Apple Maps style
/// none / follow / follow with heading, which the canvas updates when you pan or rotate);
/// simpler embeddings (calibration) use `follow` + `orientation` as before.
struct MapCanvasView: UIViewRepresentable {
    let map: MapDocument
    let tileStore: TileStore
    /// Draw the map dark (`MapAppearance.dark`): dark tiles, dark sheet and overlay colours to match.
    var darkMap: Bool = false
    var orientation: MapOrientationMode = .northUp
    var userLocation: CLLocation? = nil
    /// Compass heading in degrees (true north), if available.
    var heading: Double? = nil
    var track: [TrackPoint] = []
    var showTrack: Bool = true
    var pois: [MapPOI] = []
    var showPOIs: Bool = true
    var selectedPOIID: UUID? = nil
    var markers: [CanvasMarker] = []
    /// Camera follows the user's position. The canvas sets it to false when the user pans.
    /// Ignored when `tracking` is given.
    @Binding var follow: Bool
    /// Tap on the map (base-image pixel), when not hitting a POI.
    var onTap: ((CGPoint) -> Void)? = nil
    /// Long-press on the map (base-image pixel).
    var onLongPress: ((CGPoint) -> Void)? = nil
    var onSelectPOI: ((MapPOI) -> Void)? = nil
    /// Heading uncertainty in degrees (widens the heading beam).
    var headingAccuracy: Double? = nil
    var units: DistanceUnits = .metric
    /// Space taken by floating controls (including the safe area); the puck, the scale bar
    /// and the off-screen arrow stay inside the rest.
    var chromeInsets: UIEdgeInsets = .zero
    /// Apple Maps style tracking mode; the canvas writes `.none` when you pan, `.follow` when you
    /// rotate by hand or tap the compass in heading mode.
    var tracking: Binding<CanvasTrackingMode>? = nil
    /// Two-finger rotation (with `tracking`; the legacy path allows it only in `.free` orientation).
    var allowRotation: Bool = true
    var locationColor: LocationColor = .red
    /// Also draw POIs read off the map itself (they are printed on the paper already).
    var showMapLabels: Bool = true
    /// Receives the map rotation (for the compass) and takes commands (north up).
    var controller: MapCanvasController? = nil

    func makeUIView(context: Context) -> MapCanvasUIView {
        MapCanvasUIView(frame: .zero)
    }

    func updateUIView(_ uiView: MapCanvasUIView, context: Context) {
        uiView.onTap = onTap
        uiView.onLongPress = onLongPress
        uiView.onSelectPOI = onSelectPOI
        uiView.controller = controller

        let mode: CanvasTrackingMode
        let lockNorthUp: Bool
        let rotation: Bool
        if let tracking {
            mode = tracking.wrappedValue
            lockNorthUp = false
            rotation = allowRotation
            uiView.onTrackingChange = { value in
                // Never write SwiftUI state during a view update.
                DispatchQueue.main.async { tracking.wrappedValue = value }
            }
        } else {
            mode = follow ? (orientation == .headingUp ? .followWithHeading : .follow) : .none
            lockNorthUp = orientation == .northUp
            rotation = orientation == .free
            let binding = $follow
            uiView.onTrackingChange = { value in
                DispatchQueue.main.async { binding.wrappedValue = value.follows }
            }
        }

        uiView.apply(MapCanvasUIView.Inputs(
            map: map,
            tileStore: tileStore,
            darkMap: darkMap,
            tracking: mode,
            lockNorthUp: lockNorthUp,
            allowRotation: rotation,
            userLocation: userLocation,
            heading: heading,
            headingAccuracy: headingAccuracy,
            track: showTrack ? track : [],
            showTrack: showTrack,
            pois: pois,
            showPOIs: showPOIs,
            showMapLabels: showMapLabels,
            selectedPOIID: selectedPOIID,
            markers: markers,
            units: units,
            chromeInsets: chromeInsets,
            locationColor: locationColor.uiColor
        ))
    }

    static func dismantleUIView(_ uiView: MapCanvasUIView, coordinator: Coordinator) {
        uiView.onTrackingChange = nil
        uiView.controller = nil
    }
}
