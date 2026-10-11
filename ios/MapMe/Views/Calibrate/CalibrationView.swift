// OWNER: features agent. Replace the implementation; keep this public API.
import SwiftUI

/// Manual georeferencing: pair points on the map with points on a satellite map
/// (or with "I'm here" at your GPS position). Saves the fit into the map document.
struct CalibrationView: View {
    let mapID: UUID
    var onDone: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        Text("Calibrate")
    }
}
