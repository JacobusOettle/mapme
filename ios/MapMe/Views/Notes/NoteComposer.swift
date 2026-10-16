import PhotosUI
import SwiftUI
import UIKit

/// Where a photo for a note comes from.
enum NotePhotoSource: Equatable {
    case camera
    case library
}

/// How the note composer starts.
enum NoteComposerStart: Equatable {
    /// Writing a text note.
    case write
    /// Choosing a photo first: the camera or the library, opened at once when given.
    case photo(open: NotePhotoSource?)
    /// Editing a note.
    case edit(UUID)
}

/// What the composer is opened with (worked out by `NoteActions` from the screen).
struct NoteComposerSetup {
    var origin: NoteOrigin
    var start: NoteComposerStart
    /// The coordinate in the middle of the map view when the composer opened.
    var mapCentre: LatLon?
    var existing: MapNote?
    /// The places offered before any photo (a photo brings its own).
    var placement: NotePlacementChoice
}

/// The files of a photo prepared for a note being written. Removed when the composer goes away
/// without saving it, however it goes (Cancel, the map screen closing), so no photo is left
/// behind without a note.
final class NotePhotoDraft: @unchecked Sendable {
    private let urls: [URL]
    private let lock = NSLock()
    private var kept = false

    init(urls: [URL]) {
        self.urls = urls
    }

    /// The note was saved: the files stay.
    func keep() {
        lock.withLock { kept = true }
    }

    deinit {
        guard !kept else { return }
        for url in urls { try? FileManager.default.removeItem(at: url) }
    }
}

/// Writing or editing a note (docs/design/explorer.md, Notes and photos → Editor), in the map
/// screen's sheet: choose a photo (camera or library) when the note has one, then the text, the
/// place (the dropped pin, where the photo was taken, you, the map's centre), where and when, the
/// elevation from the terrain; Save / Cancel; Delete for an existing note. The photo is copied in
/// without its metadata before the editor shows it.
@MainActor
struct NoteComposer: View {
    let setup: NoteComposerSetup
    let context: MapScreenContext?
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    enum Stage: Equatable {
        case choose
        case preparing
        case edit
    }

    @State private var stage: Stage
    @State private var noteID: UUID
    @State private var text: String
    @State private var photo: NotePhoto?
    @State private var photoImage: UIImage?
    @State private var draft: NotePhotoDraft?
    @State private var placement: NotePlacementChoice
    @State private var detent: PresentationDetent
    @State private var pickedItem: PhotosPickerItem?
    @State private var showLibrary = false
    @State private var showCamera = false
    @State private var problem: String?
    @State private var elevation: Double?
    @State private var lookedUpElevation = false
    @State private var confirmDelete = false
    @State private var began = false
    @State private var saves = 0
    @FocusState private var textFocused: Bool

    init(setup: NoteComposerSetup, context: MapScreenContext?) {
        self.setup = setup
        self.context = context
        let existing = setup.existing
        let stage: Stage
        if case .photo = setup.start { stage = .choose } else { stage = .edit }
        _stage = State(initialValue: stage)
        _noteID = State(initialValue: existing?.id ?? UUID())
        _text = State(initialValue: existing?.text ?? "")
        _photo = State(initialValue: existing?.photo)
        _placement = State(initialValue: setup.placement)
        _elevation = State(initialValue: existing?.elevationM)
        _detent = State(initialValue: stage == .choose ? .medium : .large)
    }

    private var isNew: Bool { setup.existing == nil }

    var body: some View {
        NavigationStack {
            Group {
                switch stage {
                case .choose:
                    chooser
                case .preparing:
                    VStack(spacing: 12) {
                        ProgressView()
                        Text("Preparing the photo…")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .edit:
                    editor
                }
            }
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .accessibilityIdentifier("noteCancelButton")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { save() }
                        .fontWeight(.semibold)
                        .disabled(stage != .edit || !canSave)
                        .accessibilityIdentifier("noteSaveButton")
                }
            }
        }
        .presentationDetents([.medium, .large], selection: $detent)
        .interactiveDismissDisabled(stage == .preparing || (stage == .edit && hasChanges))
        .photosPicker(isPresented: $showLibrary, selection: $pickedItem, matching: .images, preferredItemEncoding: .current)
        .fullScreenCover(isPresented: $showCamera) {
            CameraPicker { image in cameraFinished(image) }
                .ignoresSafeArea()
        }
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            pickedItem = nil
            load(item)
        }
        .sensoryFeedback(.success, trigger: saves)
        .task { await begin() }
        .accessibilityIdentifier("noteComposer")
    }

    private var title: String {
        switch stage {
        case .choose, .preparing: return "Add a Photo"
        case .edit:
            if !isNew { return "Edit Note" }
            return photo == nil ? "New Note" : "Photo Note"
        }
    }

    private var trimmedText: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var canSave: Bool {
        placement.selectedCandidate != nil && (!trimmedText.isEmpty || photo != nil)
    }

    private var hasChanges: Bool {
        if let existing = setup.existing {
            return trimmedText != existing.text.trimmingCharacters(in: .whitespacesAndNewlines) || placement.selected != .current
        }
        return !trimmedText.isEmpty || photo != nil
    }

    // MARK: - Choosing a photo

    private var chooser: some View {
        ScrollView {
            VStack(spacing: 16) {
                Text(chooserPrompt)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 12) {
                    sourceButton("Take Photo", symbol: "camera.fill", id: "noteTakePhotoButton", enabled: CameraPicker.isAvailable) {
                        openCamera()
                    }
                    sourceButton("Choose Photo", symbol: "photo.on.rectangle", id: "noteChoosePhotoButton", enabled: true) {
                        problem = nil
                        showLibrary = true
                    }
                }
                if CameraPicker.isAvailable, CameraPicker.isDenied {
                    VStack(spacing: 6) {
                        Text("MapMe isn't allowed to use the camera.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                        Button("Open Settings") { CameraPicker.openSettings() }
                            .font(.footnote.weight(.semibold))
                    }
                }
                if let problem {
                    Label(problem, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(Theme.warn)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text("Photos are copied into MapMe without their location data and stay on this iPhone.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(20)
        }
        .accessibilityIdentifier("notePhotoChooser")
    }

    private var chooserPrompt: String {
        switch setup.origin {
        case .pin: return "A photo for a note at the dropped pin."
        case .here: return "A photo note: it goes where the photo was taken, or where you choose."
        }
    }

    private func sourceButton(_ title: String, symbol: String, id: String, enabled: Bool,
                              action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                Image(systemName: symbol)
                    .font(.system(size: 24, weight: .semibold))
                Text(title)
                    .font(.subheadline.weight(.semibold))
            }
            .foregroundStyle(Theme.accent)
            .frame(maxWidth: .infinity, minHeight: 96)
            .background(Color.primary.opacity(0.08), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.96))
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityIdentifier(id)
    }

    /// Opens what the composer was asked to open, once (the sheet's content appears again after
    /// the camera closes).
    private func begin() async {
        guard !began else { return }
        began = true
        switch setup.start {
        case .photo(open: .library?):
            await PresentationGate.settled()
            showLibrary = true
        case .photo(open: .camera?):
            await PresentationGate.settled()
            openCamera()
        case .write:
            try? await Task.sleep(nanoseconds: 350_000_000)
            textFocused = true
        case .edit:
            if let photo { photoImage = await model.notes.image(for: photo) }
        case .photo(open: nil):
            break
        }
    }

    private func openCamera() {
        problem = nil
        guard CameraPicker.isAvailable, !CameraPicker.isDenied else { return }
        showCamera = true
    }

    private func cameraFinished(_ image: UIImage?) {
        showCamera = false
        guard let image else {
            // Opened by "Take Photo" and cancelled: the user wanted nothing else.
            if setup.start == .photo(open: .camera), photo == nil {
                Task { @MainActor in
                    await PresentationGate.settled()
                    dismiss()
                }
            }
            return
        }
        // The camera's photo has no GPS of its own: where you were when you took it.
        let fix = model.location.location.map { LatLon($0.coordinate) }
        let heading = model.location.headingDegrees
        stage = .preparing
        Task { @MainActor in
            do {
                var prepared = try await model.notes.preparePhoto(image)
                prepared.photo.takenAt = prepared.photo.takenAt ?? Date()
                prepared.photo.headingDegrees = prepared.photo.headingDegrees ?? heading
                accept(prepared, position: fix, placement: .myLocation)
            } catch {
                fail(error)
            }
        }
    }

    private func load(_ item: PhotosPickerItem) {
        stage = .preparing
        problem = nil
        Task { @MainActor in
            do {
                // `.current` encoding: the file as stored (HEIC with its EXIF), not a JPEG
                // transcoded without the location.
                guard let data = try await item.loadTransferable(type: Data.self) else { throw NoteError.unreadablePhoto }
                let prepared = try await model.notes.preparePhoto(data)
                accept(prepared, position: prepared.exifPosition, placement: .exif)
            } catch {
                fail(error)
            }
        }
    }

    private func fail(_ error: Error) {
        problem = (error as? LocalizedError)?.errorDescription ?? "This photo couldn't be read."
        stage = .choose
        detent = .medium
    }

    /// The prepared photo becomes the note's: places offered again with its position.
    private func accept(_ prepared: PreparedPhoto, position: LatLon?, placement photoPlacement: PhotoPlacement) {
        let photo = prepared.photo
        draft = NotePhotoDraft(urls: [model.notes.photoURL(for: photo),
                                      model.notes.photosDirectory.appendingPathComponent(photo.thumbnailFileName)])
        self.photo = photo
        let fix = model.location.location.map { LatLon($0.coordinate) }
        placement = NotePlacementChoice.forNew(origin: setup.origin, hasPhoto: true, photoPosition: position,
                                               photoPlacement: photoPlacement, fix: fix, mapCentre: setup.mapCentre,
                                               units: model.settings.units)
        stage = .edit
        detent = .large
        Task { @MainActor in
            photoImage = await model.notes.image(for: photo)
        }
    }

    // MARK: - Editing

    private var editor: some View {
        let units = model.settings.units
        let here = model.location.location.map { LatLon($0.coordinate) }
        let chosen = placement.selectedCandidate
        let prompt: String = photo == nil ? "What's here?" : "Add a caption"
        return Form {
            if photo != nil {
                Section {
                    photoPreview
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            Section {
                TextField(prompt, text: $text, axis: .vertical)
                    .lineLimit(3...12)
                    .focused($textFocused)
                    .accessibilityIdentifier("noteTextField")
            } footer: {
                Text("The first line is the note's title.")
            }
            if placement.candidates.count > 1 {
                Section {
                    Picker("Place", selection: $placement.selected) {
                        ForEach(placement.candidates) { candidate in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(candidate.title)
                                Text(placeDetail(candidate, here: here, units: units))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .monospacedDigit()
                            }
                            .tag(candidate.kind)
                        }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                    .accessibilityIdentifier("notePlacePicker")
                } header: {
                    Text("Place")
                } footer: {
                    if let message = placement.message { Text(message) }
                }
            } else if let message = placement.message {
                Section {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            Section("Details") {
                if let chosen {
                    LabeledContent("Position") {
                        Text(GeoFormat.coordinate(chosen.position, format: model.settings.coordinateFormat))
                            .monospacedDigit()
                    }
                    if let here {
                        LabeledContent("From you") {
                            Text(distanceText(from: here, to: chosen.position, units: units))
                                .monospacedDigit()
                        }
                    }
                }
                LabeledContent("Elevation") {
                    Text(elevationText(units: units))
                        .monospacedDigit()
                }
                if let taken = photo?.takenAt {
                    LabeledContent("Taken", value: taken.formatted(date: .abbreviated, time: .shortened))
                }
            }
            if !isNew {
                Section {
                    Button(role: .destructive) {
                        confirmDelete = true
                    } label: {
                        Label("Delete Note", systemImage: "trash")
                    }
                    .accessibilityIdentifier("noteEditorDeleteButton")
                }
            }
        }
        .scrollContentBackground(.hidden)
        .task(id: chosen?.position) {
            await lookUpElevation(at: chosen?.position)
        }
        .confirmationDialog("Delete this note?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Note", role: .destructive) { deleteNote() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(photo == nil ? "The note is removed from this iPhone." : "The note and its photo are removed from this iPhone.")
        }
        .accessibilityIdentifier("noteEditor")
    }

    private var photoPreview: some View {
        ZStack {
            if let photoImage {
                Image(uiImage: photoImage)
                    .resizable()
                    .scaledToFit()
            } else {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(Theme.surfaceRaised)
                    .frame(height: 200)
                    .overlay(ProgressView())
            }
        }
        .frame(maxWidth: .infinity, maxHeight: 300)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityLabel("The note's photo")
        .accessibilityIdentifier("notePhotoPreview")
    }

    private func placeDetail(_ candidate: NotePlaceCandidate, here: LatLon?, units: DistanceUnits) -> String {
        if let here { return distanceText(from: here, to: candidate.position, units: units) }
        return GeoFormat.coordinate(candidate.position, format: model.settings.coordinateFormat)
    }

    private func distanceText(from here: LatLon, to p: LatLon, units: DistanceUnits) -> String {
        let d = Geodesy.distance(here, p)
        if d < 5 { return "Where you are" }
        return "\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(Geodesy.bearing(here, p))) of you"
    }

    private func elevationText(units: DistanceUnits) -> String {
        if let elevation { return GeoFormat.elevation(elevation, units: units) }
        return lookedUpElevation ? "Needs terrain (offline)" : "…"
    }

    /// The terrain's elevation at the chosen place (kept from the note while it stays put).
    private func lookUpElevation(at position: LatLon?) async {
        guard let position else { return }
        if let existing = setup.existing, existing.position == position, let ele = existing.elevationM {
            elevation = ele
            lookedUpElevation = true
            return
        }
        elevation = nil
        lookedUpElevation = false
        let ele = await model.notes.elevation(at: position)
        guard !Task.isCancelled else { return }
        elevation = ele
        lookedUpElevation = true
    }

    // MARK: - Save and delete

    private func save() {
        guard let chosen = placement.selectedCandidate, canSave else { return }
        var savedPhoto = photo
        if chosen.kind != .current { savedPhoto?.placement = chosen.placement }
        let now = Date()
        var note = model.geoItems.note(id: noteID)
            ?? MapNote(id: noteID, position: chosen.position, text: trimmedText, createdAt: now, updatedAt: now, photo: savedPhoto)
        note.text = trimmedText
        note.photo = savedPhoto
        if note.position != chosen.position {
            note.position = chosen.position
            note.elevationM = nil
        }
        if note.elevationM == nil, let elevation, lookedUpElevation { note.elevationM = elevation }
        note.updatedAt = now
        model.notes.save(note)
        draft?.keep()
        saves += 1
        if let context {
            context.droppedPin = nil
            context.selectedForeignPOI = nil
            context.selectedItem = GeoItemRef(kind: .note, id: note.id)
        }
        dismiss()
    }

    private func deleteNote() {
        if let context, context.selectedItem == GeoItemRef(kind: .note, id: noteID) { context.selectedItem = nil }
        model.notes.delete(noteID)
        dismiss()
    }
}
