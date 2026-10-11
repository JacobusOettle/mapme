// OWNER: map agent. Replace the implementation; keep this public API
// (CalibrationView from the features agent also embeds MapCanvasView).
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

    func makeUIView(context: Context) -> UIView { UIView() }
    func updateUIView(_ uiView: UIView, context: Context) {}
}
