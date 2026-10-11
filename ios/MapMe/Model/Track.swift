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

extension TrackPoint {
    init(_ location: CLLocation) {
        self.init(lat: location.coordinate.latitude,
                  lon: location.coordinate.longitude,
                  altitude: location.verticalAccuracy >= 0 ? location.altitude : nil,
                  timestamp: location.timestamp,
                  horizontalAccuracy: location.horizontalAccuracy >= 0 ? location.horizontalAccuracy : nil)
    }
}

struct Track: Codable, Hashable, Identifiable {
    var id: UUID = UUID()
    var name: String
    var startedAt: Date
    var endedAt: Date?
    var points: [TrackPoint] = []

    /// Total length in metres.
    var distance: Double { Self.distance(of: points) }
    var duration: TimeInterval { (endedAt ?? points.last?.timestamp ?? startedAt).timeIntervalSince(startedAt) }
    /// Total ascent in metres (smoothed).
    var ascent: Double { Self.ascent(of: points.compactMap(\.altitude)) }

    /// Elevation change must exceed this before it counts, so GPS altitude noise is not summed as climbing.
    static let ascentHysteresis = 3.0

    static func distance(of points: [TrackPoint]) -> Double {
        guard points.count > 1 else { return 0 }
        var total = 0.0
        for i in 1..<points.count {
            total += Geodesy.distance(points[i - 1].latLon, points[i].latLon)
        }
        return total
    }

    /// Sum of climbs, counting a change only once it exceeds the hysteresis from the last reference level.
    static func ascent(of altitudes: [Double]) -> Double {
        guard var reference = altitudes.first else { return 0 }
        var total = 0.0
        for a in altitudes.dropFirst() {
            if a - reference >= ascentHysteresis {
                total += a - reference
                reference = a
            } else if reference - a >= ascentHysteresis {
                reference = a
            }
        }
        return total
    }

    /// GPX 1.1 document.
    func gpx() -> String {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        var out = """
        <?xml version="1.0" encoding="UTF-8"?>
        <gpx version="1.1" creator="MapMe" xmlns="http://www.topografix.com/GPX/1/1" \
        xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" \
        xsi:schemaLocation="http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd">
          <metadata>
            <name>\(Self.xmlEscaped(name))</name>
            <time>\(iso.string(from: startedAt))</time>
          </metadata>
          <trk>
            <name>\(Self.xmlEscaped(name))</name>
            <trkseg>

        """
        for p in points {
            out += "      <trkpt lat=\"\(String(format: "%.7f", p.lat))\" lon=\"\(String(format: "%.7f", p.lon))\">"
            if let ele = p.altitude {
                out += "<ele>\(String(format: "%.1f", ele))</ele>"
            }
            out += "<time>\(iso.string(from: p.timestamp))</time></trkpt>\n"
        }
        out += """
            </trkseg>
          </trk>
        </gpx>

        """
        return out
    }

    static func xmlEscaped(_ s: String) -> String {
        var r = ""
        r.reserveCapacity(s.count)
        for c in s {
            switch c {
            case "&": r += "&amp;"
            case "<": r += "&lt;"
            case ">": r += "&gt;"
            case "\"": r += "&quot;"
            case "'": r += "&apos;"
            default: r.append(c)
            }
        }
        return r
    }
}
