import QuartzCore
import SwiftUI
import UIKit

// The maps feature on the map screen (docs/design/explorer.md, Map auto-switching, Off-map
// continuation). Stubs: the maps feature fills them in (and may split them into more files).

/// Side column: the maps covering you (a badge with their count), switch to one, the world view.
/// Also drives auto-switching while the screen shows. Empty until filled in.
@MainActor
struct MapsHereButton: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// Under the title when you are off the sheet: "You've walked off Grootkloof · showing terrain",
/// with the maps covering you or the world view.
@MainActor
struct WalkedOffMapBanner: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// The sheet's edge, a hairline where the terrain beyond it shows.
@MainActor
final class MapEdgeOverlay: CanvasOverlay {
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

/// The places your other maps read off their sheets (and their OpenStreetMap places), on this map
/// and the terrain beyond it.
@MainActor
final class OtherMapsPOIOverlay: CanvasOverlay {
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

/// Terrain contours beyond the sheet's edge, in the map's pixel space (a CATiledLayer under the
/// sheet, tiles drawn by the core's `mm_terrain_render`).
@MainActor
final class OffMapTerrainLayer: CanvasMapSpaceLayer {
    let view = UIView()
    let isBelowSheet = true
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        view.isHidden = true
    }

    func mapDidChange(_ context: CanvasMapContext) {}
}

/// Settings → Maps: switch maps automatically, terrain beyond the sheet, other maps' places.
@MainActor
struct MapsSettingsSection: View {
    var body: some View {
        EmptyView()
    }
}
