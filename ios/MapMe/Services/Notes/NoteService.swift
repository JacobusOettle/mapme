import CoreLocation
import Foundation
import Observation
import UIKit

enum NoteError: Error, LocalizedError, Equatable {
    case unreadablePhoto
    case notImplemented

    var errorDescription: String? {
        switch self {
        case .unreadablePhoto: return "This photo couldn't be read."
        case .notImplemented: return "Notes aren't available yet."
        }
    }
}

/// A photo ready to become a note: re-encoded for the store, with what its metadata said.
struct PreparedPhoto: Sendable {
    var photo: NotePhoto
    /// Where the photo says it was taken (EXIF GPS), if anywhere.
    var exifPosition: LatLon?
}

/// Notes and photos on the map (docs/design/explorer.md, Notes and photos): photos from the
/// camera or the library, re-encoded without metadata into the geo item store, placed by their
/// GPS position or at you; thumbnails for the map. Nothing leaves the device. Use from the main
/// thread; image work runs in the background.
@MainActor
@Observable
final class NoteService {
    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Re-encodes `data` (JPEG/HEIC from the camera or the library) into the store's photos folder
    /// (≤ 2048 px, no metadata) with its thumbnail, and reads its GPS position and capture time.
    func preparePhoto(_ data: Data) async throws -> PreparedPhoto {
        throw NoteError.notImplemented
    }

    /// A small square thumbnail for the map (cached in memory), nil until loaded.
    func thumbnail(for photo: NotePhoto) -> UIImage? {
        nil
    }
}

/// Notes and photos, and the battery saver are one builder's; this is the notes half's hooks.
@MainActor
enum NotesFeature {
    static func attach(_ model: AppModel) {
        model.notes.attach(model)
    }

    /// Debug launch arguments: `-seedExplorerItems` (a note with a generated photo near the sample).
    static func appDidLaunch(_ model: AppModel) async {}
}
