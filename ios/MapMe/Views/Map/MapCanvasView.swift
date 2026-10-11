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
/// (CATiledLayer pyramid, pan/pinch/rotate, camera modes, overlays).
struct MapCanvasView: UIViewRepresentable {
    let map: MapDocument
    let tileStore: TileStore
    var nightMode: Bool = false
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
    @Binding var follow: Bool
    /// Tap on the map (base-image pixel), when not hitting a POI.
    var onTap: ((CGPoint) -> Void)? = nil
    /// Long-press on the map (base-image pixel).
    var onLongPress: ((CGPoint) -> Void)? = nil
    var onSelectPOI: ((MapPOI) -> Void)? = nil
    /// Heading uncertainty in degrees (widens the heading cone).
    var headingAccuracy: Double? = nil
    var units: DistanceUnits = .metric
    /// Space taken by floating controls (including the safe area); the user's dot, the scale bar
    /// and the off-screen arrow stay inside the rest.
    var chromeInsets: UIEdgeInsets = .zero

    func makeUIView(context: Context) -> MapCanvasUIView {
        MapCanvasUIView(frame: .zero)
    }

    func updateUIView(_ uiView: MapCanvasUIView, context: Context) {
        uiView.onTap = onTap
        uiView.onLongPress = onLongPress
        uiView.onSelectPOI = onSelectPOI
        let binding = $follow
        uiView.onFollowChange = { value in
            // Never write SwiftUI state during a view update.
            DispatchQueue.main.async { binding.wrappedValue = value }
        }
        uiView.apply(MapCanvasUIView.Inputs(
            map: map,
            tileStore: tileStore,
            nightMode: nightMode,
            orientation: orientation,
            userLocation: userLocation,
            heading: heading,
            headingAccuracy: headingAccuracy,
            track: showTrack ? track : [],
            showTrack: showTrack,
            pois: pois,
            showPOIs: showPOIs,
            selectedPOIID: selectedPOIID,
            markers: markers,
            follow: follow,
            units: units,
            chromeInsets: chromeInsets
        ))
    }

    static func dismantleUIView(_ uiView: MapCanvasUIView, coordinator: Coordinator) {
        uiView.onFollowChange = nil
    }
}
