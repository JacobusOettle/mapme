import SwiftUI

/// All your maps on Earth (docs/design/explorer.md, World view): their footprints with the map
/// drawn in, your routes, waypoints, notes and track, the terrain (also offline), and you; tap a
/// map to open it. Pushed from the library (globe button) and from "Maps here". A placeholder
/// until the maps feature fills it in.
@MainActor
struct WorldView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            VStack(spacing: 12) {
                Image(systemName: "globe.europe.africa.fill")
                    .font(.system(size: 44, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text("World view")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)
                Text("\(model.library.maps.filter(\.isGeoreferenced).count) located maps")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("worldView")
        .navigationTitle("World")
        .navigationBarTitleDisplayMode(.inline)
    }
}
