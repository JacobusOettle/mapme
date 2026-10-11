// OWNER: pipeline agent. Replace the implementation; keep this public API.
import SwiftUI

/// Home screen: imported maps (nearest / "you are on this map" first), import buttons.
struct LibraryView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Text("Maps")
    }
}
