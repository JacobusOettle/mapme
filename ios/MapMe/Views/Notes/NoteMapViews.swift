import QuartzCore
import SwiftUI
import UIKit

// Notes and photos on the map screen (docs/design/explorer.md, Notes and photos). Stubs: the notes
// feature fills them in (and may split them into more files).

/// Side column: a note at your position, take a photo, choose a photo. Empty until filled in.
@MainActor
struct AddNoteButton: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// A selected note: its photo, text, when and where; edit, share the photo, delete.
@MainActor
struct NoteCard: View {
    let id: UUID
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        EmptyView()
    }
}

/// Notes and photo thumbnails on the canvas (above the map's own POIs).
@MainActor
final class NoteOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.markers
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
    }

    func mapDidChange(_ context: CanvasMapContext) {}

    func render(_ frame: CanvasFrame) {}
}

/// The dropped pin's note and photo actions.
@MainActor
enum NoteActions {
    /// Opens the note editor for a note at `position`.
    static func addNote(at position: LatLon, context: MapScreenContext, model: AppModel) {}

    /// Takes or picks a photo for a note at `position` (its own GPS position wins when it has one
    /// and the user keeps it).
    static func addPhoto(at position: LatLon, context: MapScreenContext, model: AppModel) {}
}
