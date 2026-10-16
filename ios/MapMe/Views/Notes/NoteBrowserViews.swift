import SwiftUI
import UIKit

/// All your notes (the note button's "All Notes"): those on the map shown first, then the others;
/// nearest first when there is a fix, newest first otherwise. Tap one to show its card on the map;
/// swipe to delete.
@MainActor
struct NotesListView: View {
    let context: MapScreenContext?
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let here = model.location.location.map { LatLon($0.coordinate) }
        let groups = Self.groups(notes: model.geoItems.notes, map: context.flatMap { model.library.map(id: $0.mapID) },
                                 here: here)
        let units = model.settings.units
        let othersTitle: String = groups.onMap.isEmpty ? "Notes" : "Elsewhere"
        NavigationStack {
            List {
                if groups.onMap.isEmpty, groups.elsewhere.isEmpty {
                    ContentUnavailableView {
                        Label("No Notes Yet", systemImage: "note.text")
                    } description: {
                        Text("Add a note or a photo with the note button, or long-press the map and choose Note or Photo.")
                    }
                    .listRowBackground(Color.clear)
                }
                if !groups.onMap.isEmpty {
                    Section("On This Map") {
                        ForEach(groups.onMap) { note in row(note, here: here, units: units) }
                    }
                }
                if !groups.elsewhere.isEmpty {
                    Section(othersTitle) {
                        ForEach(groups.elsewhere) { note in row(note, here: here, units: units) }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle("Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .fontWeight(.semibold)
                        .accessibilityIdentifier("notesListDone")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .accessibilityIdentifier("notesList")
    }

    private func row(_ note: MapNote, here: LatLon?, units: DistanceUnits) -> some View {
        Button {
            select(note.id)
        } label: {
            HStack(spacing: 12) {
                if let photo = note.photo {
                    NoteThumbnail(note: note, photo: photo, size: 44)
                } else {
                    NoteGlyph(size: 36)
                        .frame(width: 44, height: 44)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(note.title)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Text(NoteFormat.subtitle(note, here: here, units: units))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            Button(role: .destructive) {
                if let context, context.selectedItem == GeoItemRef(kind: .note, id: note.id) { context.selectedItem = nil }
                model.notes.delete(note.id)
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
        .accessibilityIdentifier("noteRow")
    }

    private func select(_ id: UUID) {
        if let context {
            context.droppedPin = nil
            context.selectedForeignPOI = nil
            context.selectedItem = GeoItemRef(kind: .note, id: id)
        }
        dismiss()
    }

    /// The notes on `map`'s sheet and the others, each nearest first (newest first without a fix).
    static func groups(notes: [MapNote], map: MapDocument?, here: LatLon?) -> (onMap: [MapNote], elsewhere: [MapNote]) {
        var sorted = notes
        if let here {
            sorted.sort { Geodesy.distance(here, $0.position) < Geodesy.distance(here, $1.position) }
        }
        guard let map, let georef = map.georef else { return ([], sorted) }
        let projector = Georeference.Projector(georef)
        let sheet = CGRect(origin: .zero, size: map.pixelSize)
        var onMap: [MapNote] = []
        var elsewhere: [MapNote] = []
        for note in sorted {
            let p = projector.pixel(for: note.position)
            if p.x.isFinite, p.y.isFinite, sheet.contains(p) {
                onMap.append(note)
            } else {
                elsewhere.append(note)
            }
        }
        return (onMap, elsewhere)
    }
}

/// A note's photo full screen: pinch or double-tap to zoom, drag to look around; Share sends the
/// re-encoded photo (no location in it).
@MainActor
struct NotePhotoViewer: View {
    let noteID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var scale: CGFloat = 1
    @State private var lastScale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var lastOffset: CGSize = .zero
    @State private var showsChrome = true

    static let maxScale: CGFloat = 6

    var body: some View {
        let note = model.geoItems.note(id: noteID)
        ZStack {
            Color.black.ignoresSafeArea()
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .scaleEffect(scale)
                    .offset(offset)
                    .gesture(zoom.simultaneously(with: pan))
                    .onTapGesture(count: 2) {
                        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                            if scale > 1.01 {
                                resetZoom()
                            } else {
                                scale = 2.5
                                lastScale = 2.5
                            }
                        }
                    }
                    .onTapGesture {
                        withAnimation(.easeInOut(duration: 0.2)) { showsChrome.toggle() }
                    }
                    .accessibilityLabel(note.map { "Photo: \($0.title)" } ?? "Photo")
            } else if note?.photo == nil {
                Text("This note has no photo.")
                    .foregroundStyle(Color.white.opacity(0.7))
            } else {
                ProgressView()
                    .tint(.white)
            }
            if showsChrome {
                chrome(note)
                    .transition(.opacity)
            }
        }
        .statusBarHidden(!showsChrome)
        .task {
            guard let photo = note?.photo else { return }
            image = await model.notes.image(for: photo)
        }
        .accessibilityIdentifier("notePhotoViewer")
    }

    private func chrome(_ note: MapNote?) -> some View {
        VStack(spacing: 0) {
            HStack {
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(MapRoundButtonStyle())
                .accessibilityLabel("Close photo")
                .accessibilityIdentifier("notePhotoClose")
                Spacer()
                if let note, let photo = note.photo {
                    ShareLink(item: model.notes.photoURL(for: photo), subject: Text(note.title), message: Text(note.text)) {
                        Image(systemName: "square.and.arrow.up")
                    }
                    .buttonStyle(MapRoundButtonStyle())
                    .accessibilityLabel("Share photo")
                    .accessibilityIdentifier("notePhotoShare")
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            Spacer()
            if let note {
                VStack(alignment: .leading, spacing: 4) {
                    Text(note.title)
                        .font(.headline)
                    Text(NoteFormat.subtitle(note, here: model.location.location.map { LatLon($0.coordinate) },
                                             units: model.settings.units))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
            }
        }
    }

    private var zoom: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                scale = min(max(lastScale * value.magnification, 1), Self.maxScale)
            }
            .onEnded { _ in
                lastScale = scale
                if scale <= 1.01 {
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) { resetZoom() }
                }
            }
    }

    private var pan: some Gesture {
        DragGesture(minimumDistance: 10)
            .onChanged { value in
                guard scale > 1.01 else { return }
                offset = CGSize(width: lastOffset.width + value.translation.width,
                                height: lastOffset.height + value.translation.height)
            }
            .onEnded { _ in
                lastOffset = offset
            }
    }

    private func resetZoom() {
        scale = 1
        lastScale = 1
        offset = .zero
        lastOffset = .zero
    }
}
