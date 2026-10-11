// OWNER: map agent. Replace the implementation; keep this public API.
import SwiftUI

/// The live map: tiled map canvas, red GPS dot + heading, dotted trail, POIs, HUD and controls.
struct MapScreen: View {
    let mapID: UUID
    @Environment(AppModel.self) private var model

    var body: some View {
        Text("Map")
    }
}
