// OWNER: features agent. Replace the implementation; keep this public API.
import SwiftUI

/// Bottom sheet listing points of interest on this map, nearest first, with
/// distance and a bearing arrow relative to the user's heading.
struct NearbySheet: View {
    let mapID: UUID
    /// Called when the user taps a POI (the map highlights it and draws a guide line).
    var onSelect: (MapPOI) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        Text("Nearby")
    }
}
