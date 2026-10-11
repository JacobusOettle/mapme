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

/// Navigation root: the map library, pushing map screens by id. One full-screen cover hosts the
/// whole import flow (corner editor for photos of paper maps, then progress), so the screens
/// swap in place instead of one cover dismissing while the next presents.
struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        // A plain reference for the cover binding's setter (not the local @Bindable var below,
        // which a possibly @Sendable closure must not capture).
        let app = self.model
        @Bindable var model = model
        let flowActive = model.isImportFlowActive
        NavigationStack(path: $model.path) {
            LibraryView()
                .navigationDestination(for: UUID.self) { id in
                    MapScreen(mapID: id)
                }
        }
        .fullScreenCover(isPresented: Binding(get: { flowActive }, set: { shown in
            // Only a dismissal the model didn't ask for cancels the flow.
            if !shown, app.isImportFlowActive { app.cancelImport() }
        })) {
            ImportFlowView()
        }
    }
}

/// Contents of the import cover: the corner editor while a photo waits for its corners, the
/// progress screen otherwise.
struct ImportFlowView: View {
    @Environment(AppModel.self) private var model
    /// Kept on screen while the cover slides away after Cancel in the editor.
    @State private var lastPhoto: PendingPhoto?

    var body: some View {
        let photo = shownPhoto
        ZStack {
            if let photo {
                CornerEditorView(photo: photo,
                                 onCancel: { model.cancelCorners() },
                                 onDone: { model.confirmCorners($0) },
                                 detect: { await model.detectCorners(in: photo) })
                    .id(photo.id)
                    .transition(.opacity)
            } else {
                ImportProgressView()
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.3), value: photo?.id)
        .onAppear { lastPhoto = model.pendingPhoto }
        .onChange(of: model.pendingPhoto?.id) { _, _ in
            if let pending = model.pendingPhoto {
                lastPhoto = pending
            } else if model.importJob != nil {
                // Done: the progress screen takes over.
                lastPhoto = nil
            }
        }
    }

    private var shownPhoto: PendingPhoto? {
        if let pending = model.pendingPhoto { return pending }
        // Cancelled in the editor: nothing is pending any more, keep the editor while the cover closes.
        if model.importJob == nil { return lastPhoto }
        return nil
    }
}
