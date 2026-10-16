import CoreGraphics
import CoreLocation
import Foundation
import Observation

/// How well one map covers a position.
struct MapCoverage: Equatable, Sendable {
    var mapID: UUID
    /// Ground metres per map pixel at the position (smaller = more detailed).
    var metersPerPixel: Double
    /// Distance from the position to the sheet's nearest edge, ground metres (negative outside).
    var insideMarginM: Double
}

/// Which of your maps cover where you are, and when to switch to a more detailed one as you walk
/// (docs/design/explorer.md, Map auto-switching). Use from the main thread.
@MainActor
@Observable
final class MapSwitcher {
    /// Maps covering your position, most detailed first (updated as you move).
    private(set) var mapsHere: [MapCoverage] = []

    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// How `map` covers `position` (nil without a georeference or when the position does not
    /// project): through the georeference and its warp, the sheet's rectangle.
    nonisolated static func coverage(of map: MapDocument, at position: LatLon) -> MapCoverage? {
        // Filled in by the maps feature.
        nil
    }

    /// The map the screen showing `currentID` should switch to now, or nil to stay (the rules:
    /// covers you with a margin, ≥ 1.5× more detailed, for 20 s; only while following; hysteresis).
    func suggestion(currentID: UUID, fix: CLLocation, tracking: CanvasTrackingMode, now: Date) -> UUID? {
        // Filled in by the maps feature.
        nil
    }
}

/// How far a map is continued beyond its sheet's edge with terrain (contours, the 3D view's
/// surroundings): the core's rule (`terrain::continuation_margin_m`: half the sheet's long side,
/// 3…15 km), in the map's pixels, kept where its projection holds.
enum MapContinuation {
    /// Map pixels added on every side; 0 = no continuation (not located, the setting off).
    static func marginPixels(for map: MapDocument, enabled: Bool) -> CGFloat {
        // Filled in by the maps feature.
        0
    }
}
