import AVFoundation
import SwiftUI
import UIKit

// Notes and photos on the map screen (docs/design/explorer.md, Notes and photos): the side
// column's note button, the selected note's card and the actions the dropped pin and the card
// start. The canvas overlay is in NoteOverlay.swift, the editor in NoteComposer.swift, the list
// and the photo viewer in NoteBrowserViews.swift.

/// Side column: a note at your position, take a photo, choose a photo, all notes.
@MainActor
struct AddNoteButton: View {
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model

    var body: some View {
        Menu {
            Button {
                NoteActions.noteHere(context: context, model: model)
            } label: {
                Label("Note Here", systemImage: "note.text.badge.plus")
            }
            .accessibilityIdentifier("addNoteHereButton")
            Button {
                NoteActions.takePhoto(context: context, model: model)
            } label: {
                Label("Take Photo", systemImage: "camera")
            }
            .disabled(!CameraPicker.isAvailable)
            .accessibilityIdentifier("takePhotoNoteButton")
            Button {
                NoteActions.choosePhoto(context: context, model: model)
            } label: {
                Label("Choose Photo", systemImage: "photo.on.rectangle")
            }
            .accessibilityIdentifier("choosePhotoNoteButton")
            Section {
                Button {
                    NoteActions.showList(context: context)
                } label: {
                    Label("All Notes", systemImage: "list.bullet")
                }
                .accessibilityIdentifier("notesListButton")
            }
        } label: {
            Image(systemName: "square.and.pencil")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .floatingChrome(Circle(), interactive: true)
        .accessibilityLabel("Notes and photos")
        .accessibilityIdentifier("addNoteButton")
    }
}

/// A selected note: its photo, text, when and where; edit, share, delete.
@MainActor
struct NoteCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model
    @State private var confirmDelete = false

    var body: some View {
        if let note = model.geoItems.note(id: id) {
            card(note)
        } else {
            // Deleted elsewhere (the list): nothing to show any more.
            Color.clear
                .frame(height: 0)
                .onAppear {
                    if context.selectedItem == GeoItemRef(kind: .note, id: id) { context.selectedItem = nil }
                }
        }
    }

    private func card(_ note: MapNote) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                if let photo = note.photo {
                    Button {
                        NoteActions.showPhoto(note.id, context: context)
                    } label: {
                        NoteThumbnail(note: note, photo: photo, size: 56)
                    }
                    .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                    .accessibilityLabel("Show photo")
                    .accessibilityIdentifier("notePhotoButton")
                } else {
                    NoteGlyph(size: 40)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text(note.title)
                        .font(.headline)
                        .lineLimit(2)
                    Text(NoteFormat.subtitle(note, here: model.location.location.map { LatLon($0.coordinate) },
                                             units: model.settings.units))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                        .lineLimit(2)
                }
                Spacer(minLength: 8)
                Button {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { context.selectedItem = nil }
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(Color.primary.opacity(0.1), in: Circle())
                }
                .buttonStyle(PressScaleButtonStyle())
                .accessibilityLabel("Close note")
                .accessibilityIdentifier("noteCardClose")
            }
            if let rest = NoteFormat.body(note) {
                Text(rest)
                    .font(.subheadline)
                    .foregroundStyle(Color.primary.opacity(0.85))
                    .lineLimit(4)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                action("Edit", symbol: "pencil", id: "noteEditButton") {
                    NoteActions.edit(note.id, context: context, model: model)
                }
                shareAction(note)
                action("Delete", symbol: "trash", id: "noteDeleteButton", role: .destructive) {
                    confirmDelete = true
                }
            }
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("noteCard")
        .confirmationDialog("Delete this note?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Note", role: .destructive) {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    context.selectedItem = nil
                    model.notes.delete(note.id)
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(note.photo == nil ? "The note is removed from this iPhone." : "The note and its photo are removed from this iPhone.")
        }
    }

    /// The photo (re-encoded, without its location) with the note's text, or the note's text and
    /// coordinates.
    @ViewBuilder
    private func shareAction(_ note: MapNote) -> some View {
        if let photo = note.photo {
            ShareLink(item: model.notes.photoURL(for: photo), subject: Text(note.title), message: Text(note.text)) {
                tileLabel("Share", symbol: "square.and.arrow.up", role: nil)
            }
            .buttonStyle(PressScaleButtonStyle(scale: 0.95))
            .accessibilityIdentifier("noteShareButton")
        } else {
            ShareLink(item: NoteFormat.shareText(note), subject: Text(note.title)) {
                tileLabel("Share", symbol: "square.and.arrow.up", role: nil)
            }
            .buttonStyle(PressScaleButtonStyle(scale: 0.95))
            .accessibilityIdentifier("noteShareButton")
        }
    }

    private func action(_ title: String, symbol: String, id: String, role: ButtonRole? = nil,
                        perform: @escaping () -> Void) -> some View {
        Button(role: role, action: perform) {
            tileLabel(title, symbol: symbol, role: role)
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.95))
        .accessibilityIdentifier(id)
    }

    private func tileLabel(_ title: String, symbol: String, role: ButtonRole?) -> some View {
        VStack(spacing: 4) {
            Image(systemName: symbol)
                .font(.system(size: 16, weight: .semibold))
            Text(title)
                .font(.caption.weight(.semibold))
        }
        .foregroundStyle(role == .destructive ? Theme.bad : Theme.accent)
        .frame(maxWidth: .infinity, minHeight: 52)
        .background(Color.primary.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

/// The dropped pin's note and photo actions, and the note button's.
@MainActor
enum NoteActions {
    /// Opens the note editor for a note at `position`.
    static func addNote(at position: LatLon, context: MapScreenContext, model: AppModel) {
        present(.write, origin: .pin(position), context: context, model: model)
    }

    /// Takes or picks a photo for a note at `position` (its own GPS position wins when it has one
    /// and the user keeps it).
    static func addPhoto(at position: LatLon, context: MapScreenContext, model: AppModel) {
        present(.photo(open: nil), origin: .pin(position), context: context, model: model)
    }

    /// The note button's "Note Here": a text note at you (the map's centre without a fix).
    static func noteHere(context: MapScreenContext, model: AppModel) {
        present(.write, origin: .here, context: context, model: model)
    }

    /// The note button's "Take Photo": the camera at once; the note goes where you took it.
    static func takePhoto(context: MapScreenContext, model: AppModel) {
        guard !CameraPicker.isDenied else {
            context.show(MapToast(text: "MapMe isn't allowed to use the camera", symbol: "camera", actionTitle: "Settings",
                                  action: { CameraPicker.openSettings() }))
            return
        }
        present(.photo(open: .camera), origin: .here, context: context, model: model)
    }

    /// The note button's "Choose Photo": the photo library at once; the note goes where the photo
    /// was taken, or where the user says.
    static func choosePhoto(context: MapScreenContext, model: AppModel) {
        present(.photo(open: .library), origin: .here, context: context, model: model)
    }

    /// Edits a note (text, place; delete).
    static func edit(_ id: UUID, context: MapScreenContext, model: AppModel) {
        present(.edit(id), origin: .here, context: context, model: model)
    }

    /// All notes, those on this map first.
    static func showList(context: MapScreenContext) {
        context.sheet = MapPresentation { [weak context] in
            AnyView(NotesListView(context: context))
        }
    }

    /// The note's photo full screen.
    static func showPhoto(_ id: UUID, context: MapScreenContext) {
        context.cover = MapPresentation {
            AnyView(NotePhotoViewer(noteID: id))
        }
    }

    private static func present(_ start: NoteComposerStart, origin: NoteOrigin, context: MapScreenContext, model: AppModel) {
        let fix = model.location.location.map { LatLon($0.coordinate) }
        let centre = mapCentre(context: context, model: model)
        var existing: MapNote?
        if case .edit(let id) = start {
            existing = model.geoItems.note(id: id)
            guard existing != nil else { return }
        }
        let placement = existing.map { NotePlacementChoice.forExisting($0, fix: fix) }
            ?? NotePlacementChoice.forNew(origin: origin, hasPhoto: false, photoPosition: nil, fix: fix, mapCentre: centre,
                                          units: model.settings.units)
        let setup = NoteComposerSetup(origin: origin, start: start, mapCentre: centre, existing: existing, placement: placement)
        context.sheet = MapPresentation { [weak context] in
            AnyView(NoteComposer(setup: setup, context: context))
        }
    }

    /// The coordinate in the middle of the map view; the sheet's centre before the canvas has a
    /// camera; nil when the map is not located.
    static func mapCentre(context: MapScreenContext, model: AppModel) -> LatLon? {
        if let centre = context.controller.cameraHandoff(to: context.mapID)?.center { return centre }
        guard let map = model.library.map(id: context.mapID), let g = map.georef else { return nil }
        let p = g.latLon(forPixel: CGPoint(x: map.pixelSize.width / 2, y: map.pixelSize.height / 2))
        return p.lat.isFinite && p.lon.isFinite ? p : nil
    }
}

/// Texts about a note shared by the card, the list and the editor.
enum NoteFormat {
    /// "21 Jun 2026 at 11:42 · 120 m NE · 1 385 m"
    static func subtitle(_ note: MapNote, here: LatLon?, units: DistanceUnits) -> String {
        var parts = [(note.photo?.takenAt ?? note.createdAt).formatted(date: .abbreviated, time: .shortened)]
        if let here {
            let d = Geodesy.distance(here, note.position)
            parts.append(d < 5 ? "Here" : "\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(Geodesy.bearing(here, note.position)))")
        }
        if let ele = note.elevationM { parts.append(GeoFormat.elevation(ele, units: units)) }
        return parts.joined(separator: " · ")
    }

    /// The text after the title line, if any.
    static func body(_ note: MapNote) -> String? {
        let lines = note.text.split(separator: "\n", omittingEmptySubsequences: true)
        guard lines.count > 1 else { return nil }
        let rest = lines.dropFirst().joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return rest.isEmpty ? nil : rest
    }

    /// A text note to share: its text and where it is.
    static func shareText(_ note: MapNote) -> String {
        let place = String(format: "%.5f, %.5f", note.position.lat, note.position.lon)
        return note.text.isEmpty ? place : "\(note.text)\n\n\(place)"
    }
}

/// A note's photo as a rounded square thumbnail (an amber placeholder while it loads).
@MainActor
struct NoteThumbnail: View {
    let note: MapNote
    let photo: NotePhoto
    var size: CGFloat = 44
    @Environment(AppModel.self) private var model

    var body: some View {
        ZStack {
            if let image = model.notes.thumbnail(for: photo) {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Theme.note.opacity(0.35)
                Image(systemName: "photo")
                    .font(.system(size: size * 0.36, weight: .semibold))
                    .foregroundStyle(Color.white)
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.22, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: size * 0.22, style: .continuous).strokeBorder(Color.white.opacity(0.18), lineWidth: 0.5))
        .accessibilityHidden(true)
    }
}

/// A text note's glyph: an amber disc with a note symbol, as on the map.
struct NoteGlyph: View {
    var size: CGFloat = 40

    var body: some View {
        ZStack {
            Circle()
                .fill(Color.white)
            Circle()
                .fill(Theme.note.gradient)
                .padding(size * 0.07)
            Image(systemName: "note.text")
                .font(.system(size: size * 0.4, weight: .semibold))
                .foregroundStyle(Color.white)
        }
        .frame(width: size, height: size)
        .shadow(color: Color.black.opacity(0.25), radius: 2, y: 1)
        .accessibilityHidden(true)
    }
}

/// The camera (`UIImagePickerController`): a photo for a note.
@MainActor
struct CameraPicker: UIViewControllerRepresentable {
    /// The photo taken, or nil when cancelled.
    var onFinish: (UIImage?) -> Void

    /// There is a camera (never in the Simulator).
    static var isAvailable: Bool { UIImagePickerController.isSourceTypeAvailable(.camera) }

    /// The user said no to the camera (or a profile forbids it): explain and offer Settings
    /// instead of an empty camera screen.
    static var isDenied: Bool {
        let status = AVCaptureDevice.authorizationStatus(for: .video)
        return status == .denied || status == .restricted
    }

    static func openSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = ["public.image"]
        picker.cameraCaptureMode = .photo
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {
        context.coordinator.onFinish = onFinish
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onFinish: onFinish)
    }

    @MainActor
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        var onFinish: (UIImage?) -> Void

        init(onFinish: @escaping (UIImage?) -> Void) {
            self.onFinish = onFinish
        }

        func imagePickerController(_ picker: UIImagePickerController,
                                   didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            onFinish(info[.originalImage] as? UIImage)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            onFinish(nil)
        }
    }
}
