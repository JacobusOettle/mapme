import CoreLocation
import Foundation
import Observation

/// Records the breadcrumb trail. The current track is always shown on the map
/// (even when not "recording" a saved track) so you can see where you walked.
/// Use from the main thread.
@Observable
final class TrackRecorder {
    /// True while the user has explicitly started a recording (saved to disk, background updates).
    private(set) var isRecording = false
    /// Points walked this session (shown as the dotted trail); becomes the saved track when recording.
    private(set) var current: Track = Track(name: "Current", startedAt: Date())
    /// Saved tracks, newest first.
    private(set) var tracks: [Track] = []

    /// Fixes worse than this are ignored (mock fixes always pass).
    static let maxAccuracy = 35.0
    /// A new point is appended after moving this far…
    static let minSpacing = 4.0
    /// …or after this long, whichever comes first.
    static let maxInterval: TimeInterval = 20
    static let autosaveInterval: TimeInterval = 30

    let directory: URL

    @ObservationIgnored private weak var location: LocationService?
    @ObservationIgnored private var autosaveTimer: Timer?
    @ObservationIgnored private var dirty = false

    /// Application Support/Tracks.
    static var defaultDirectory: URL {
        URL.applicationSupportDirectory.appendingPathComponent("Tracks", isDirectory: true)
    }

    init(directory: URL? = nil) {
        self.directory = directory ?? Self.defaultDirectory
        tracks = Self.loadTracks(from: self.directory)
    }

    func attach(location: LocationService) {
        self.location = location
        let mocked = location.isMocked
        location.addLocationObserver { [weak self] fix in
            self?.ingest(fix, trusted: mocked)
        }
    }

    func start() {
        guard !isRecording else { return }
        let now = Date()
        let name = "Track " + now.formatted(date: .abbreviated, time: .shortened)
        var track = Track(name: name, startedAt: now)
        if let last = location?.location, isAcceptable(last, trusted: location?.isMocked ?? false) {
            track.points.append(TrackPoint(last))
        }
        current = track
        isRecording = true
        dirty = true
        location?.setBackgroundUpdates(true)
        let timer = Timer(timeInterval: Self.autosaveInterval, repeats: true) { [weak self] _ in
            self?.autosave()
        }
        RunLoop.main.add(timer, forMode: .common)
        autosaveTimer = timer
    }

    /// Stops and saves the recording.
    func stop() {
        guard isRecording else { return }
        isRecording = false
        autosaveTimer?.invalidate()
        autosaveTimer = nil
        location?.setBackgroundUpdates(false)
        var finished = current
        finished.endedAt = finished.points.last?.timestamp ?? Date()
        current = finished
        if finished.points.count >= 2 {
            write(finished)
            tracks.removeAll { $0.id == finished.id }
            tracks.insert(finished, at: 0)
        } else {
            try? FileManager.default.removeItem(at: fileURL(for: finished.id))
        }
        dirty = false
    }

    /// Clear the on-screen trail.
    func clearCurrent() {
        if isRecording {
            current.points.removeAll()
            dirty = true
        } else {
            current = Track(name: "Current", startedAt: Date())
        }
    }

    func delete(_ track: Track) {
        tracks.removeAll { $0.id == track.id }
        try? FileManager.default.removeItem(at: fileURL(for: track.id))
    }

    /// Writes a .gpx file to a temporary location for sharing.
    func gpxFile(for track: Track) throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("GPX", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let url = folder.appendingPathComponent(Self.fileSafeName(track.name)).appendingPathExtension("gpx")
        try Data(track.gpx().utf8).write(to: url, options: .atomic)
        return url
    }

    // MARK: - Internals

    private func isAcceptable(_ fix: CLLocation, trusted: Bool) -> Bool {
        trusted || (fix.horizontalAccuracy >= 0 && fix.horizontalAccuracy <= Self.maxAccuracy)
    }

    private func ingest(_ fix: CLLocation, trusted: Bool) {
        guard isAcceptable(fix, trusted: trusted) else { return }
        let point = TrackPoint(fix)
        if let last = current.points.last {
            let moved = Geodesy.distance(last.latLon, point.latLon)
            let waited = point.timestamp.timeIntervalSince(last.timestamp)
            guard moved >= Self.minSpacing || waited > Self.maxInterval else { return }
        }
        current.points.append(point)
        if isRecording { dirty = true }
    }

    private func autosave() {
        guard isRecording, dirty else { return }
        dirty = false
        write(current)
    }

    private func fileURL(for id: UUID) -> URL {
        directory.appendingPathComponent(id.uuidString).appendingPathExtension("json")
    }

    private func write(_ track: Track) {
        let url = fileURL(for: track.id)
        let folder = directory
        // Encoding a few thousand points is cheap; the disk write goes off the main thread.
        guard let data = try? Self.makeEncoder().encode(track) else { return }
        DispatchQueue.global(qos: .utility).async {
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try? data.write(to: url, options: .atomic)
        }
    }

    static func loadTracks(from directory: URL) -> [Track] {
        let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let loaded = files.filter { $0.pathExtension == "json" }.compactMap { url -> Track? in
            guard let data = try? Data(contentsOf: url) else { return nil }
            return try? decoder.decode(Track.self, from: data)
        }
        return loaded.sorted { $0.startedAt > $1.startedAt }
    }

    static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return encoder
    }

    static func fileSafeName(_ name: String) -> String {
        let bad = CharacterSet(charactersIn: "/\\:?%*|\"<>")
        let cleaned = name.components(separatedBy: bad).joined(separator: "-").trimmingCharacters(in: .whitespaces)
        return cleaned.isEmpty ? "Track" : cleaned
    }
}
