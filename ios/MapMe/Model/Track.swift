// OWNER: map agent. Replace the implementation; keep this public API.
import CoreLocation
import Foundation

struct TrackPoint: Codable, Hashable {
    var lat: Double
    var lon: Double
    var altitude: Double?
    var timestamp: Date
    var horizontalAccuracy: Double?

    var latLon: LatLon { LatLon(lat: lat, lon: lon) }
}

struct Track: Codable, Hashable, Identifiable {
    var id: UUID = UUID()
    var name: String
    var startedAt: Date
    var endedAt: Date?
    var points: [TrackPoint] = []

    /// Total length in metres.
    var distance: Double { 0 }
    var duration: TimeInterval { (endedAt ?? points.last?.timestamp ?? startedAt).timeIntervalSince(startedAt) }
    /// Total ascent in metres (smoothed).
    var ascent: Double { 0 }

    /// GPX 1.1 document.
    func gpx() -> String { "" }
}
