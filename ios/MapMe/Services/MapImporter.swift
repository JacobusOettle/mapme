// OWNER: pipeline agent. Replace the implementation; keep this public API.
import CoreGraphics
import Foundation

struct ImportedMap {
    var document: MapDocument
    /// Full-resolution base raster (tile level 0), for georeferencing.
    var baseImage: CGImage
}

enum ImportError: Error, LocalizedError {
    case unreadable(String)
    case unsupported(String)
    var errorDescription: String? {
        switch self {
        case .unreadable(let s): return "Could not read the file: \(s)"
        case .unsupported(let s): return "Unsupported file: \(s)"
        }
    }
}

/// Turns a PDF/image into a stored MapDocument + tile pyramid + thumbnail.
final class MapImporter {
    let library: MapLibrary

    init(library: MapLibrary) { self.library = library }

    /// `progress(fraction 0…1, step description)` may be called from any thread.
    func importFile(at url: URL, progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        throw ImportError.unsupported(url.lastPathComponent)
    }

    func importImageData(_ data: Data, suggestedName: String, progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        throw ImportError.unsupported(suggestedName)
    }

    /// Re-create the base raster of an existing map (for re-locating).
    func loadBaseImage(for map: MapDocument) async throws -> CGImage {
        throw ImportError.unreadable(map.name)
    }
}
