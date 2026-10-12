import Foundation
import Observation

/// On-disk store of imported maps: Application Support/Maps/<uuid>/{meta.json, source.*, tiles/,
/// tiles-dark-v2/, thumb.jpg, thumb-dark.jpg, dark-profile.json}. Use from the main thread (the
/// path helpers only read `rootURL` and are safe anywhere).
@Observable
final class MapLibrary {
    /// Most recently opened/created first.
    private(set) var maps: [MapDocument] = []
    let rootURL: URL

    static let metaFileName = "meta.json"

    /// Application Support/Maps.
    static var defaultRootURL: URL {
        URL.applicationSupportDirectory.appendingPathComponent("Maps", isDirectory: true)
    }

    init(rootURL: URL? = nil) {
        self.rootURL = rootURL ?? Self.defaultRootURL
        reload()
    }

    func reload() {
        let fm = FileManager.default
        try? fm.createDirectory(at: rootURL, withIntermediateDirectories: true)
        let folders = (try? fm.contentsOfDirectory(at: rootURL, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles])) ?? []
        let decoder = Self.makeDecoder()
        var loaded: [MapDocument] = []
        for folder in folders {
            let meta = folder.appendingPathComponent(Self.metaFileName)
            guard let data = try? Data(contentsOf: meta),
                  let doc = try? decoder.decode(MapDocument.self, from: data)
            else { continue }
            loaded.append(doc)
        }
        maps = Self.sorted(loaded)
    }

    func map(id: UUID) -> MapDocument? { maps.first { $0.id == id } }
    func directory(for id: UUID) -> URL { rootURL.appendingPathComponent(id.uuidString, isDirectory: true) }
    func sourceURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent(map.sourceFileName) }
    func thumbnailURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent("thumb.jpg") }
    /// The thumbnail drawn dark with the map's dark style (made after import, or on first use).
    func darkThumbnailURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent("thumb-dark.jpg") }
    /// The map's dark-map profile (`MapMeCore.darkProfile`), computed once from an overview.
    func darkProfileURL(for map: MapDocument) -> URL { directory(for: map.id).appendingPathComponent("dark-profile.json") }
    /// Light tiles, or the core's dark rendering of them (`tiles-dark-v2`, made on first view and
    /// in the background after import).
    func tilesDirectory(for map: MapDocument, dark: Bool) -> URL {
        directory(for: map.id).appendingPathComponent(dark ? "tiles-dark-v2" : "tiles", isDirectory: true)
    }
    /// Night tiles of older versions (a plain inversion); removed when found.
    func legacyDarkTilesDirectory(for map: MapDocument) -> URL {
        directory(for: map.id).appendingPathComponent("tiles-dark", isDirectory: true)
    }

    /// Insert or update (writes meta.json).
    func save(_ map: MapDocument) {
        do {
            try Self.write(map, to: directory(for: map.id))
        } catch {
            print("MapLibrary: could not save \(map.id): \(error)")
        }
        var list = maps
        if let i = list.firstIndex(where: { $0.id == map.id }) {
            list[i] = map
        } else {
            list.append(map)
        }
        maps = Self.sorted(list)
    }

    func delete(_ map: MapDocument) {
        let folder = directory(for: map.id)
        try? FileManager.default.removeItem(at: folder)
        // A background dark render of this map stops; whatever it wrote meanwhile goes too.
        DarkPrerender.cancel(map.id) { try? FileManager.default.removeItem(at: folder) }
        maps.removeAll { $0.id == map.id }
    }

    func rename(_ map: MapDocument, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        var updated = self.map(id: map.id) ?? map
        guard updated.name != trimmed else { return }
        updated.name = trimmed
        save(updated)
    }

    /// Mark as opened now (re-sorts the list).
    func touch(_ id: UUID) {
        guard var updated = map(id: id) else { return }
        updated.lastOpenedAt = Date()
        save(updated)
    }

    // MARK: - Helpers

    static func write(_ map: MapDocument, to folder: URL) throws {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let data = try makeEncoder().encode(map)
        try data.write(to: folder.appendingPathComponent(metaFileName), options: .atomic)
    }

    static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }

    static func makeDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }

    /// Most recent (opened or created) first; ties broken by id so the order is stable.
    static func sorted(_ list: [MapDocument]) -> [MapDocument] {
        list.sorted { a, b in
            let da = a.lastOpenedAt ?? a.createdAt
            let db = b.lastOpenedAt ?? b.createdAt
            if da != db { return da > db }
            return a.id.uuidString < b.id.uuidString
        }
    }
}
