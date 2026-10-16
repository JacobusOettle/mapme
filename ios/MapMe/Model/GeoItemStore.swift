import Foundation
import Observation

/// The user's geo items, on disk in Application Support/GeoItems:
///
///     waypoints.json        {"version": 1, "waypoints": [Waypoint]}
///     notes.json            {"version": 1, "notes": [MapNote]}
///     routes/<uuid>.json    {"version": 1, "route": Route}   (one file each: tracks can be long)
///     photos/<uuid>.jpg     a note's photo (≤ 2048 px, no metadata), photos/<uuid>-thumb.jpg
///
/// Items are global (latitude/longitude, no map). Changes are published at once and written
/// atomically on a background queue, newest snapshot last. A file that does not decode is moved
/// aside (`<name>.unreadable-<date>`), never overwritten, and that kind starts empty. Use from the
/// main thread.
@MainActor
@Observable
final class GeoItemStore {
    private(set) var waypoints: [Waypoint] = []
    /// Newest first.
    private(set) var routes: [Route] = []
    /// Newest first.
    private(set) var notes: [MapNote] = []
    /// Bumped on every change: a cheap thing for overlays to observe.
    private(set) var revision = 0

    let directory: URL
    static let currentVersion = 1

    @ObservationIgnored private let writer = GeoItemWriter()

    /// Application Support/GeoItems.
    nonisolated static var defaultDirectory: URL {
        URL.applicationSupportDirectory.appendingPathComponent("GeoItems", isDirectory: true)
    }

    init(directory: URL? = nil) {
        self.directory = directory ?? Self.defaultDirectory
        reload()
    }

    var routesDirectory: URL { directory.appendingPathComponent("routes", isDirectory: true) }
    var photosDirectory: URL { directory.appendingPathComponent("photos", isDirectory: true) }
    private var waypointsURL: URL { directory.appendingPathComponent("waypoints.json") }
    private var notesURL: URL { directory.appendingPathComponent("notes.json") }

    func routeURL(_ id: UUID) -> URL { routesDirectory.appendingPathComponent(id.uuidString).appendingPathExtension("json") }
    func photoURL(for photo: NotePhoto) -> URL { photosDirectory.appendingPathComponent(photo.fileName) }
    func thumbnailURL(for photo: NotePhoto) -> URL { photosDirectory.appendingPathComponent(photo.thumbnailFileName) }

    // MARK: - Loading

    /// Reads everything from disk (blocking; the files are small, routes one file each).
    func reload() {
        let fm = FileManager.default
        try? fm.createDirectory(at: routesDirectory, withIntermediateDirectories: true)
        try? fm.createDirectory(at: photosDirectory, withIntermediateDirectories: true)
        waypoints = Self.load(WaypointsFile.self, from: waypointsURL)?.waypoints ?? []
        notes = (Self.load(NotesFile.self, from: notesURL)?.notes ?? []).sorted { $0.createdAt > $1.createdAt }
        let files = (try? fm.contentsOfDirectory(at: routesDirectory, includingPropertiesForKeys: nil)) ?? []
        routes = files.filter { $0.pathExtension == "json" }
            .compactMap { Self.load(RouteFile.self, from: $0)?.route }
            .sorted { $0.createdAt > $1.createdAt }
        revision += 1
    }

    // MARK: - Lookup

    func waypoint(id: UUID) -> Waypoint? { waypoints.first { $0.id == id } }
    func route(id: UUID) -> Route? { routes.first { $0.id == id } }
    func note(id: UUID) -> MapNote? { notes.first { $0.id == id } }

    func waypoints(in bounds: GeoBounds) -> [Waypoint] { waypoints.filter { bounds.contains($0.position) } }
    func notes(in bounds: GeoBounds) -> [MapNote] { notes.filter { bounds.contains($0.position) } }

    /// Routes with a point inside `bounds` (O(points)).
    func routes(touching bounds: GeoBounds) -> [Route] {
        routes.filter { route in route.points.contains { bounds.contains($0.latLon) } }
    }

    // MARK: - Changes

    /// Adds or replaces a waypoint (by id).
    func save(_ waypoint: Waypoint) {
        if let i = waypoints.firstIndex(where: { $0.id == waypoint.id }) {
            waypoints[i] = waypoint
        } else {
            waypoints.append(waypoint)
        }
        changed()
        writeWaypoints()
    }

    /// Adds or replaces several waypoints in one write (GPX import, migration).
    func save(_ list: [Waypoint]) {
        guard !list.isEmpty else { return }
        for waypoint in list {
            if let i = waypoints.firstIndex(where: { $0.id == waypoint.id }) {
                waypoints[i] = waypoint
            } else {
                waypoints.append(waypoint)
            }
        }
        changed()
        writeWaypoints()
    }

    func deleteWaypoint(_ id: UUID) {
        guard waypoints.contains(where: { $0.id == id }) else { return }
        waypoints.removeAll { $0.id == id }
        changed()
        writeWaypoints()
    }

    /// Adds or replaces a route (by id); `updatedAt` is left as given.
    func save(_ route: Route) {
        if let i = routes.firstIndex(where: { $0.id == route.id }) {
            routes[i] = route
        } else {
            routes.insert(route, at: 0)
        }
        changed()
        if let data = try? Self.encoder.encode(RouteFile(version: Self.currentVersion, route: route)) {
            writer.write(data, to: routeURL(route.id))
        }
    }

    func deleteRoute(_ id: UUID) {
        guard routes.contains(where: { $0.id == id }) else { return }
        routes.removeAll { $0.id == id }
        changed()
        writer.remove(routeURL(id))
    }

    /// Adds or replaces a note (by id). Its photo file must already be in `photosDirectory`.
    func save(_ note: MapNote) {
        if let i = notes.firstIndex(where: { $0.id == note.id }) {
            let old = notes[i]
            notes[i] = note
            if let photo = old.photo, photo.id != note.photo?.id { removePhotoFiles(photo) }
        } else {
            notes.insert(note, at: 0)
        }
        changed()
        writeNotes()
    }

    /// Deletes a note and its photo files.
    func deleteNote(_ id: UUID) {
        guard let doomed = self.note(id: id) else { return }
        notes.removeAll { $0.id == id }
        if let photo = doomed.photo { removePhotoFiles(photo) }
        changed()
        writeNotes()
    }

    /// Waits until every write queued so far is on disk (tests, before the app is suspended).
    func flush() {
        writer.flush()
    }

    // MARK: - Internals

    private func changed() {
        revision += 1
    }

    private func writeWaypoints() {
        guard let data = try? Self.encoder.encode(WaypointsFile(version: Self.currentVersion, waypoints: waypoints)) else { return }
        writer.write(data, to: waypointsURL)
    }

    private func writeNotes() {
        guard let data = try? Self.encoder.encode(NotesFile(version: Self.currentVersion, notes: notes)) else { return }
        writer.write(data, to: notesURL)
    }

    private func removePhotoFiles(_ photo: NotePhoto) {
        writer.remove(photoURL(for: photo))
        writer.remove(thumbnailURL(for: photo))
    }

    private struct WaypointsFile: Codable {
        var version: Int
        var waypoints: [Waypoint]
    }

    private struct NotesFile: Codable {
        var version: Int
        var notes: [MapNote]
    }

    private struct RouteFile: Codable {
        var version: Int
        var route: Route
    }

    /// Dates are written as seconds since 2001 (Foundation's own form): they read back exactly,
    /// so an item compares equal to itself after a reload.
    static let encoder = JSONEncoder()
    static let decoder = JSONDecoder()

    /// The file decoded, or nil when it is absent; an unreadable file is moved aside, not lost.
    private static func load<T: Decodable>(_ type: T.Type, from url: URL) -> T? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        do {
            return try decoder.decode(type, from: data)
        } catch {
            let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-")
            let aside = url.deletingPathExtension().appendingPathExtension("unreadable-\(stamp).json")
            try? FileManager.default.moveItem(at: url, to: aside)
            print("GeoItemStore: \(url.lastPathComponent) could not be read (\(error)); kept as \(aside.lastPathComponent)")
            return nil
        }
    }
}

/// Writes the store's files on one serial background queue (atomic replaces), in order.
final class GeoItemWriter: @unchecked Sendable {
    private let queue = DispatchQueue(label: "com.jacobusoettle.mapme.geo-items", qos: .utility)

    func write(_ data: Data, to url: URL) {
        queue.async {
            do {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try data.write(to: url, options: .atomic)
            } catch {
                print("GeoItemStore: could not write \(url.lastPathComponent): \(error)")
            }
        }
    }

    func remove(_ url: URL) {
        queue.async {
            try? FileManager.default.removeItem(at: url)
        }
    }

    func flush() {
        queue.sync {}
    }
}
