import Darwin
import Foundation

/// Terrain tiles kept on the device for good: offline regions and the terrain around located
/// maps (docs/design/explorer.md, Offline regions). Unlike the terrain cache (Caches/dem, least
/// recently used, 400 MB, which iOS may purge), pinned tiles live in Application Support/Terrain
/// (excluded from backups) and stay while any owner's tile ranges cover them:
///
///     Terrain/tiles/{z}/{x}/{y}.png
///     Terrain/owners/{region|map}-<uuid>.json   {"version": 1, "ranges": [TileRange], ...}
///
/// `DEMService` reads pinned tiles before its cache and the network, so matching, profiles, the
/// terrain beyond a sheet and the 3D view work offline from them. Thread-safe.
///
/// Crash safety: an owner's ranges are written before any of its tiles, so a tile on disk always
/// has an owner (or is the leftover of a release); a release first renames the owner's manifest
/// to `<stem>.releasing.json`, deletes the tiles no one else covers, then the manifest, and a
/// release cut short is finished at the next launch (`finishInterruptedReleases`).
final class PinnedTerrainStore: @unchecked Sendable {
    static let shared = PinnedTerrainStore()

    let rootURL: URL

    /// Manifest format written by this version.
    static let manifestVersion = 1
    private static let releasingSuffix = ".releasing.json"

    private let lock = NSLock()
    /// Serialises releases with range changes, so a tile deleted by a release is never one an
    /// owner registered meanwhile (that owner's download finds it absent and fetches it again).
    private let mutationLock = NSLock()
    private var loaded = false
    private var ownerRanges: [TerrainOwner: [TileRange]] = [:]
    /// Every owner's ranges by zoom: "is it pinned" scans only the zoom's few rectangles
    /// (one per region, a handful per located map), microseconds.
    private var byZoom: [Int: [TileRange]] = [:]

    /// Application Support/Terrain.
    static var defaultRootURL: URL {
        URL.applicationSupportDirectory.appendingPathComponent("Terrain", isDirectory: true)
    }

    init(rootURL: URL? = nil) {
        self.rootURL = rootURL ?? Self.defaultRootURL
    }

    var tilesDirectory: URL { rootURL.appendingPathComponent("tiles", isDirectory: true) }
    var ownersDirectory: URL { rootURL.appendingPathComponent("owners", isDirectory: true) }

    func tileURL(for t: TileID) -> URL {
        tilesDirectory
            .appendingPathComponent("\(t.z)", isDirectory: true)
            .appendingPathComponent("\(t.x)", isDirectory: true)
            .appendingPathComponent("\(t.y).png")
    }

    func manifestURL(for owner: TerrainOwner) -> URL {
        ownersDirectory.appendingPathComponent(owner.fileStem + ".json")
    }

    // MARK: - Lookups

    /// The pinned file of a tile, if it is on the device.
    func pinnedTile(_ t: TileID) -> URL? {
        guard covers(t) else { return nil }
        let url = tileURL(for: t)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// Some owner's ranges cover the tile (whether or not it has been downloaded yet).
    func covers(_ t: TileID) -> Bool {
        lock.withLock {
            loadIfNeeded()
            return byZoom[t.z]?.contains { $0.contains(t) } ?? false
        }
    }

    /// The tile's file exists (owned or not). Blocking (one `stat`).
    func hasTile(_ t: TileID) -> Bool {
        FileManager.default.fileExists(atPath: tileURL(for: t).path)
    }

    /// Size of the tile's file, nil when absent. Blocking.
    func tileBytes(_ t: TileID) -> Int64? {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: tileURL(for: t).path),
              let size = attributes[.size] as? NSNumber
        else { return nil }
        return size.int64Value
    }

    /// The owners with ranges on the device.
    var owners: [TerrainOwner] {
        lock.withLock {
            loadIfNeeded()
            return Array(ownerRanges.keys)
        }
    }

    /// The ranges recorded for `owner` (nil: none).
    func ranges(for owner: TerrainOwner) -> [TileRange]? {
        lock.withLock {
            loadIfNeeded()
            return ownerRanges[owner]
        }
    }

    // MARK: - Owners

    /// Records `ranges` as `owner`'s (replacing what it had); tiles still need downloading. Tiles
    /// the owner had before and no longer covers go unless another owner keeps them. Blocking
    /// (writes the manifest, may delete files): run off the main thread.
    func setRanges(_ newRanges: [TileRange], for owner: TerrainOwner) {
        mutationLock.withLock {
            let old = lock.withLock { () -> [TileRange]? in
                loadIfNeeded()
                return ownerRanges[owner]
            }
            writeManifest(newRanges, for: owner)
            lock.withLock {
                ownerRanges[owner] = newRanges
                rebuildIndex()
            }
            guard let old, old != newRanges else { return }
            // Re-located map: the trailhead moved. Drop what only the old area needed.
            deleteUncovered(old)
        }
    }

    /// Drops `owner` and deletes the tiles no other owner covers. Blocking: run off the main thread.
    func release(_ owner: TerrainOwner) {
        mutationLock.withLock {
            let old = lock.withLock { () -> [TileRange]? in
                loadIfNeeded()
                let old = ownerRanges.removeValue(forKey: owner)
                rebuildIndex()
                return old
            }
            let manifest = manifestURL(for: owner)
            let journal = ownersDirectory.appendingPathComponent(owner.fileStem + Self.releasingSuffix)
            let fm = FileManager.default
            if fm.fileExists(atPath: manifest.path) {
                try? fm.removeItem(at: journal)
                try? fm.moveItem(at: manifest, to: journal)
            }
            if let old { deleteUncovered(old) }
            try? fm.removeItem(at: journal)
        }
    }

    /// Finishes releases a crash or a kill cut short (their journals are still there). Blocking.
    func finishInterruptedReleases() {
        let fm = FileManager.default
        guard let names = try? fm.contentsOfDirectory(atPath: ownersDirectory.path) else { return }
        for name in names where name.hasSuffix(Self.releasingSuffix) {
            let url = ownersDirectory.appendingPathComponent(name)
            mutationLock.withLock {
                if let manifest = Self.readManifest(at: url) {
                    deleteUncovered(manifest.ranges)
                }
                try? fm.removeItem(at: url)
            }
        }
    }

    // MARK: - Tiles

    /// Writes a downloaded tile into place (atomically). Throws on a full disk and other write
    /// errors. False when no owner covers the tile any more (released while it downloaded): it is
    /// not kept. Any thread.
    @discardableResult
    func store(_ data: Data, for t: TileID) throws -> Bool {
        let url = tileURL(for: t)
        do {
            try data.write(to: url, options: .atomic)
        } catch {
            // The folder may not exist yet, or a concurrent release just removed it as empty.
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try data.write(to: url, options: .atomic)
        }
        return keepIfCovered(url, t)
    }

    /// Moves a file (a tile from the terrain cache, a background download) into place. False when
    /// it could not be moved, or no owner covers the tile any more. Any thread.
    @discardableResult
    func adopt(_ file: URL, for t: TileID) -> Bool {
        let url = tileURL(for: t)
        let fm = FileManager.default
        try? fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        if fm.fileExists(atPath: url.path) {
            try? fm.removeItem(at: file)
            return true
        }
        do {
            try fm.moveItem(at: file, to: url)
        } catch {
            return false
        }
        return keepIfCovered(url, t)
    }

    /// A release drops its owner from the index before it deletes files, so a tile written after
    /// its owner's release passed it is caught here: covered still, or deleted now. Either way no
    /// tile is left on disk without an owner.
    private func keepIfCovered(_ url: URL, _ t: TileID) -> Bool {
        if covers(t) { return true }
        try? FileManager.default.removeItem(at: url)
        return false
    }

    /// Bytes on disk of all pinned tiles. Blocking.
    func totalBytes() -> Int64 {
        StorageUsage.directorySize(tilesDirectory)
    }

    /// How many of `ranges`' tiles are on the device, and their bytes. Blocking (one `stat` per
    /// tile: ~0.1 s for 10 000 tiles).
    func presence(of ranges: [TileRange]) -> (tiles: Int, bytes: Int64) {
        var tiles = 0
        var bytes: Int64 = 0
        for range in ranges {
            for t in range.tiles {
                if let size = tileBytes(t) {
                    tiles += 1
                    bytes += size
                }
            }
        }
        return (tiles, bytes)
    }

    // MARK: - Private

    /// Deletes the tiles of `old` that no current owner covers, then the folders left empty.
    private func deleteUncovered(_ old: [TileRange]) {
        let fm = FileManager.default
        var folders = Set<String>()
        for range in old {
            for t in range.tiles where !covers(t) {
                let url = tileURL(for: t)
                if (try? fm.removeItem(at: url)) != nil {
                    folders.insert(url.deletingLastPathComponent().path)
                }
            }
        }
        // rmdir only removes empty folders: never another owner's tiles.
        for folder in folders {
            _ = rmdir(folder)
        }
    }

    private struct Manifest: Codable {
        var version: Int
        var owner: String
        var ranges: [TileRange]
        var updatedAt: Date?
    }

    private func writeManifest(_ ranges: [TileRange], for owner: TerrainOwner) {
        let fm = FileManager.default
        try? fm.createDirectory(at: ownersDirectory, withIntermediateDirectories: true)
        Self.excludeFromBackup(rootURL)
        let manifest = Manifest(version: Self.manifestVersion, owner: owner.fileStem, ranges: ranges, updatedAt: Date())
        guard let data = try? JSONEncoder().encode(manifest) else { return }
        try? data.write(to: manifestURL(for: owner), options: .atomic)
    }

    private static func readManifest(at url: URL) -> Manifest? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Manifest.self, from: data)
    }

    /// Reads the owners' manifests once. Call with `lock` held.
    private func loadIfNeeded() {
        guard !loaded else { return }
        loaded = true
        let fm = FileManager.default
        guard let names = try? fm.contentsOfDirectory(atPath: ownersDirectory.path) else { return }
        for name in names where name.hasSuffix(".json") && !name.hasSuffix(Self.releasingSuffix) {
            let url = ownersDirectory.appendingPathComponent(name)
            let stem = String(name.dropLast(5))
            guard let owner = TerrainOwner(fileStem: stem) else { continue }
            guard let manifest = Self.readManifest(at: url) else {
                // Unreadable: keep the file for inspection, and the tiles (they stay until the
                // owner is released or the next cleanup); never overwrite it silently.
                let aside = ownersDirectory.appendingPathComponent("\(stem).unreadable-\(Int(Date().timeIntervalSince1970)).bad")
                try? fm.moveItem(at: url, to: aside)
                continue
            }
            ownerRanges[owner] = manifest.ranges
        }
        rebuildIndex()
    }

    /// Call with `lock` held.
    private func rebuildIndex() {
        var index: [Int: [TileRange]] = [:]
        for list in ownerRanges.values {
            for r in list where r.count > 0 {
                index[r.z, default: []].append(r)
            }
        }
        byZoom = index
    }

    /// Pinned terrain can be downloaded again: keep it out of iCloud and iTunes backups.
    private static func excludeFromBackup(_ url: URL) {
        var target = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        try? target.setResourceValues(values)
    }
}

extension TerrainOwner {
    /// Parses a manifest's file name stem ("region-<uuid>", "map-<uuid>").
    init?(fileStem: String) {
        if fileStem.hasPrefix("region-"), let id = UUID(uuidString: String(fileStem.dropFirst(7))) {
            self = .region(id)
        } else if fileStem.hasPrefix("map-"), let id = UUID(uuidString: String(fileStem.dropFirst(4))) {
            self = .map(id)
        } else {
            return nil
        }
    }
}
