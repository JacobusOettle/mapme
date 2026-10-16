import SwiftUI

// The 3D view (docs/design/explorer.md, 3D view). Stubs: the 3D feature fills them in.

/// Side column: the 3D button (a cube), presenting `Terrain3DScreen` full screen over the map.
/// Empty until filled in.
@MainActor
struct Terrain3DButton: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// The map draped on the terrain in 3D, with the surrounding area, POIs, routes, the track and
/// you. A placeholder until the 3D feature fills it in.
@MainActor
struct Terrain3DScreen: View {
    let mapID: UUID
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack(alignment: .topLeading) {
            Theme.background.ignoresSafeArea()
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(MapRoundButtonStyle())
            .padding(16)
            .accessibilityLabel("Close 3D view")
            .accessibilityIdentifier("terrain3DClose")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("terrain3DView")
    }
}
