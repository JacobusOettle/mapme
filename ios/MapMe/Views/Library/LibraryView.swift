import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// Home screen: imported maps (nearest / "you are on this map" first), import buttons.
struct LibraryView: View {
    @Environment(AppModel.self) private var model

    @State private var showFileImporter = false
    @State private var showPhotoPicker = false
    @State private var photoItem: PhotosPickerItem?
    @State private var showSettings = false
    @State private var renameTarget: MapDocument?
    @State private var renameText = ""
    @State private var showRename = false
    @State private var deleteTarget: MapDocument?
    @State private var showDelete = false
    @State private var calibrationTarget: MapDocument?
    @State private var openCount = 0

    struct Entry: Identifiable {
        let map: MapDocument
        let proximity: MapProximity
        var id: UUID { map.id }
    }

    /// Maps you are standing on first, then the library order (most recent first).
    static func entries(maps: [MapDocument], here: LatLon?) -> [Entry] {
        let all = maps.map { Entry(map: $0, proximity: MapProximity.of($0, from: here)) }
        return all.filter { $0.proximity.isOnMap } + all.filter { !$0.proximity.isOnMap }
    }

    var body: some View {
        let here = model.location.location.map { LatLon($0.coordinate) }
        let entries = Self.entries(maps: model.library.maps, here: here)
        ZStack {
            Theme.background.ignoresSafeArea()
            if entries.isEmpty {
                LibraryEmptyState(onImportFile: { showFileImporter = true },
                                  onImportPhoto: { showPhotoPicker = true })
                    .transition(.opacity)
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 300, maximum: 520), spacing: 16)], spacing: 16) {
                        ForEach(entries) { entry in
                            card(for: entry)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 32)
                    .animation(.spring(response: 0.45, dampingFraction: 0.85), value: entries.map { $0.id })
                }
                .scrollIndicators(.hidden)
            }
        }
        .navigationTitle("MapMe")
        .toolbar { toolbarContent }
        .fileImporter(isPresented: $showFileImporter, allowedContentTypes: [.pdf, .image]) { result in
            if case .success(let url) = result {
                model.handleIncoming(url: url)
            }
        }
        .photosPicker(isPresented: $showPhotoPicker, selection: $photoItem, matching: .images)
        .onChange(of: photoItem) { _, item in
            guard let item else { return }
            photoItem = nil
            importPhoto(item)
        }
        .alert("Rename map", isPresented: $showRename, presenting: renameTarget) { map in
            TextField("Map name", text: $renameText)
            Button("Cancel", role: .cancel) {}
            Button("Save") { model.library.rename(map, to: renameText) }
        }
        .confirmationDialog("Delete this map?", isPresented: $showDelete, titleVisibility: .visible,
                            presenting: deleteTarget) { map in
            Button("Delete \u{201C}\(map.name)\u{201D}", role: .destructive) {
                withAnimation(.easeInOut(duration: 0.3)) { model.library.delete(map) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("The map, its tiles and its location are removed from this device.")
        }
        .sheet(item: $calibrationTarget) { map in
            CalibrationView(mapID: map.id, onDone: { calibrationTarget = nil })
                .environment(model)
        }
        .sheet(isPresented: $showSettings) {
            SettingsView()
                .environment(model)
        }
        .sensoryFeedback(.impact(weight: .light), trigger: openCount)
    }

    @MainActor
    private func card(for entry: Entry) -> some View {
        let map = entry.map
        return Button {
            openCount += 1
            model.path.append(map.id)
        } label: {
            MapCardView(map: map,
                        thumbnailURL: model.library.thumbnailURL(for: map),
                        proximity: entry.proximity,
                        units: model.settings.units)
        }
        .buttonStyle(LibraryCardButtonStyle())
        .contextMenu {
            Button {
                renameText = map.name
                renameTarget = map
                showRename = true
            } label: {
                Label("Rename", systemImage: "pencil")
            }
            Button {
                model.relocate(mapID: map.id)
            } label: {
                Label("Locate again", systemImage: "scope")
            }
            Button {
                calibrationTarget = map
            } label: {
                Label("Calibrate manually", systemImage: "hand.tap")
            }
            Divider()
            Button(role: .destructive) {
                deleteTarget = map
                showDelete = true
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Button {
                showSettings = true
            } label: {
                Image(systemName: "gearshape.fill")
                    .foregroundStyle(Theme.textSecondary)
            }
            .accessibilityLabel("Settings")
            .accessibilityIdentifier("settingsButton")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button {
                    showFileImporter = true
                } label: {
                    Label("PDF or image from Files", systemImage: "folder")
                }
                Button {
                    showPhotoPicker = true
                } label: {
                    Label("Photo or screenshot", systemImage: "photo.on.rectangle.angled")
                }
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 15, weight: .bold))
                    .foregroundStyle(Color.black)
                    .frame(width: 30, height: 30)
                    .background(Theme.accent, in: Circle())
            }
            .accessibilityLabel("Import map")
            .accessibilityIdentifier("importMenu")
        }
    }

    @MainActor
    private func importPhoto(_ item: PhotosPickerItem) {
        let name = "Photo " + Date().formatted(date: .abbreviated, time: .shortened)
        let model = self.model
        Task { @MainActor in
            guard let data = try? await item.loadTransferable(type: Data.self) else { return }
            model.importImage(data: data, name: name)
        }
    }
}
