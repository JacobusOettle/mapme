import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Home screen: imported maps (nearest / "you are on this map" first), import buttons.
struct LibraryView: View {
    @Environment(AppModel.self) private var model

    @State private var showFileImporter = false
    @State private var showPhotoPicker = false
    @State private var photoItem: PhotosPickerItem?
    @State private var showScanner = false
    /// The scanned page, imported once the document camera has finished closing.
    @State private var scannedPage: UIImage?
    @State private var showSettings = false
    @State private var renameTarget: MapDocument?
    @State private var renameText = ""
    @State private var showRename = false
    @State private var deleteTarget: MapDocument?
    @State private var showDelete = false
    @State private var calibrationTarget: MapDocument?
    @State private var searchWiderTarget: MapDocument?
    /// Picked in the "Search wider" sheet; started once the sheet has closed.
    @State private var pendingWideSearch: (mapID: UUID, choice: WideSearchChoice)?
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
                                  onImportPhoto: { showPhotoPicker = true },
                                  onScan: DocumentScannerView.isSupported ? { showScanner = true } : nil)
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
        .fileImporter(isPresented: $showFileImporter, allowedContentTypes: [.pdf, .image, .gpx]) { result in
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
        .fullScreenCover(isPresented: $showScanner, onDismiss: {
            guard let page = scannedPage else { return }
            scannedPage = nil
            model.importScannedPage(page)
        }) {
            DocumentScannerView(onScan: { page in
                scannedPage = page
                showScanner = false
            }, onCancel: {
                showScanner = false
            })
            .ignoresSafeArea()
        }
        .sheet(item: $calibrationTarget) { map in
            CalibrationView(mapID: map.id, onDone: { calibrationTarget = nil })
                .environment(model)
        }
        .sheet(item: $searchWiderTarget, onDismiss: { startPendingWideSearch() }) { map in
            SearchWiderSheet(mapName: map.name,
                             previousRadiusKm: map.locateAttempt?.searchedRadiusKm ?? model.settings.searchRadiusKm,
                             hintQueries: map.locateAttempt?.hintQueries ?? [],
                             position: model.location.location.map { LatLon($0.coordinate) },
                             units: model.settings.units,
                             onStart: { choice in
                                 pendingWideSearch = (map.id, choice)
                                 searchWiderTarget = nil
                             },
                             onCancel: { searchWiderTarget = nil })
        }
        .sheet(isPresented: $showSettings) {
            SettingsView()
                .environment(model)
        }
        .samplePhotoPickerStandIn(model)
        .sensoryFeedback(.impact(weight: .light), trigger: openCount)
    }

    /// Draws a map's dark thumbnail (maps imported before dark maps get one on first view).
    @MainActor
    private static func darkThumbnailMaker(_ map: MapDocument, library: MapLibrary) -> () -> Void {
        let store = TileStore(map: map, library: library)
        return { _ = store.writeDarkThumbnail() }
    }

    @MainActor
    private func card(for entry: Entry) -> some View {
        let map = entry.map
        let library = model.library
        let dark = model.settings.mapAppearance == .dark
        return Button {
            openCount += 1
            model.path.append(.map(map.id))
        } label: {
            MapCardView(map: map,
                        thumbnailURL: dark ? library.darkThumbnailURL(for: map) : library.thumbnailURL(for: map),
                        proximity: entry.proximity,
                        units: model.settings.units,
                        makeThumbnail: dark ? Self.darkThumbnailMaker(map, library: library) : nil)
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
            if !map.isGeoreferenced, map.locateAttempt?.failure.widerSearchMayHelp ?? true {
                Button {
                    searchWiderTarget = map
                } label: {
                    Label("Search wider\u{2026}", systemImage: "location.magnifyingglass")
                }
            }
            Button {
                calibrationTarget = map
            } label: {
                Label("Calibrate manually", systemImage: "hand.tap")
            }
            if map.canAdjustCorners {
                Button {
                    model.adjustCorners(mapID: map.id)
                } label: {
                    Label("Adjust Corners", systemImage: "perspective")
                }
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
            Button {
                model.path.append(.world)
            } label: {
                Image(systemName: "globe.europe.africa.fill")
                    .foregroundStyle(Theme.textSecondary)
            }
            .accessibilityLabel("World view of your maps")
            .accessibilityIdentifier("worldViewButton")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                if DocumentScannerView.isSupported {
                    Button {
                        showScanner = true
                    } label: {
                        Label("Scan Paper Map", systemImage: "camera.viewfinder")
                    }
                }
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
                Button {
                    showFileImporter = true
                } label: {
                    Label("GPX route or waypoints", systemImage: "point.topleft.down.to.point.bottomright.curvepath")
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

    /// The search runs once the sheet has gone, so the import screen can come up over the library.
    @MainActor
    private func startPendingWideSearch() {
        guard let pending = pendingWideSearch else { return }
        pendingWideSearch = nil
        model.searchWider(mapID: pending.mapID, choice: pending.choice)
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

extension View {
    /// Debug builds with `-importSamplePhotoAfterSheet` (UI tests): a stand-in for the photo picker
    /// opens over the library; its pick closes it and starts the sample photo's import in the same
    /// moment, as a pick in the real picker does, so the import cover is asked for while a sheet is
    /// still closing. Release builds: nothing.
    @ViewBuilder
    func samplePhotoPickerStandIn(_ model: AppModel) -> some View {
        #if DEBUG
        modifier(SamplePhotoPickerStandIn(model: model))
        #else
        self
        #endif
    }
}

#if DEBUG
private struct SamplePhotoPickerStandIn: ViewModifier {
    static let launchArgument = "-importSamplePhotoAfterSheet"

    let model: AppModel
    @State private var shown = ProcessInfo.processInfo.arguments.contains(SamplePhotoPickerStandIn.launchArgument)

    func body(content: Content) -> some View {
        content.sheet(isPresented: $shown) {
            NavigationStack {
                List {
                    Button {
                        shown = false
                        model.importSamplePhoto()
                    } label: {
                        Label("Sample photo", systemImage: "photo")
                    }
                    .accessibilityIdentifier("pickSamplePhotoButton")
                }
                .navigationTitle("Photos")
                .navigationBarTitleDisplayMode(.inline)
            }
        }
    }
}
#endif
