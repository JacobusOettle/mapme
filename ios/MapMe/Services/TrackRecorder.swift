// OWNER: map agent. Replace the implementation; keep this public API.
import CoreLocation
import Foundation
import Observation

/// Records the breadcrumb trail. The current track is always shown on the map
/// (even when not "recording" a saved track) so you can see where you walked.
@Observable
final class TrackRecorder {
    /// True while the user has explicitly started a recording (saved to disk, background updates).
    private(set) var isRecording = false
    /// Points walked this session (shown as the dotted trail); becomes the saved track when recording.
    private(set) var current: Track = Track(name: "Current", startedAt: Date())
    /// Saved tracks, newest first.
    private(set) var tracks: [Track] = []

    func attach(location: LocationService) {}
    func start() {}
    /// Stops and saves the recording.
    func stop() {}
    /// Clear the on-screen trail.
    func clearCurrent() {}
    func delete(_ track: Track) {}
    /// Writes a .gpx file to a temporary location for sharing.
    func gpxFile(for track: Track) throws -> URL { FileManager.default.temporaryDirectory }
}
