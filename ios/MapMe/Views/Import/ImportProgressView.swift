// OWNER: pipeline agent. Replace the implementation; keep this public API.
import SwiftUI

/// Full-screen progress for `model.importJob` (import → locate), with result actions.
struct ImportProgressView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Text(model.importJob?.step ?? "")
    }
}
