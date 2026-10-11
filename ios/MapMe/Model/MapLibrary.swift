// OWNER: pipeline agent. Replace the implementation; keep this public API.
import Foundation
import Observation

/// On-disk store of imported maps: Application Support/Maps/<uuid>/{meta.json, source.*, tiles/, tiles-dark/, thumb.jpg}.
@Observable
final class MapLibrary {
    /// Most recently opened/created first.
    private(set) var maps: [MapDocument] = []
    let rootURL: URL

    init(rootURL: URL? = nil) {
        self.rootURL = rootURL ?? URL.applicationSupportDirectory.appendingPathComponent("Maps", isDirectory: true)
        reload()
    }

    func reload() {}
    func map(id: UUID) -> MapDocument? { maps.first { $0.id == id } }
    func directory(for id: UUID) -> URL { rootURL.appendingPathComponent(id.uuidString, isDirectory: true) }
    func sourceURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent(map.sourceFileName) }
    func thumbnailURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent("thumb.jpg") }
    func tilesDirectory(for map: MapDocument, dark: Bool) -> URL {
        directory(for: map.id).appendingPathComponent(dark ? "tiles-dark" : "tiles", isDirectory: true)
    }
    /// Insert or update (writes meta.json).
    func save(_ map: MapDocument) {}
    func delete(_ map: MapDocument) {}
    func rename(_ map: MapDocument, to name: String) {}
    /// Mark as opened now (re-sorts the list).
    func touch(_ id: UUID) {}
}
