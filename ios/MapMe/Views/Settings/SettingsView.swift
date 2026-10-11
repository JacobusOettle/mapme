// OWNER: features agent. Replace the implementation; keep this public API.
import SwiftUI

/// Preferences, saved tracks (GPX export), storage, about.
struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Text("Settings")
    }
}
