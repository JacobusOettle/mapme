import SwiftUI

@main
struct MapMeApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(.dark)
                .tint(Theme.accent)
                .onOpenURL { url in model.handleIncoming(url: url) }
                .task { await model.onLaunch() }
        }
    }
}

/// Navigation root: the map library, pushing map screens by id.
struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.path) {
            LibraryView()
                .navigationDestination(for: UUID.self) { id in
                    MapScreen(mapID: id)
                }
        }
        .fullScreenCover(item: $model.importJob) { _ in
            ImportProgressView()
        }
    }
}
