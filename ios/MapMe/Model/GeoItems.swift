import Foundation
import SwiftUI
import UIKit

// Things the user puts on Earth: waypoints, routes (planned, or imported from GPX) and notes with
// an optional photo. They are GLOBAL, kept in `GeoItemStore` by latitude/longitude and owned by
// no map: every map that covers them shows them (on the sheet and in the terrain beyond its edge),
// and so do the world view and the 3D view (docs/design/explorer.md, Data model).
//
// Persistence rule: these types are stored as JSON. A field added later must be Optional (or have
// a hand-written decoder), so files written by older versions still load.

/// The kinds of geo items.
enum GeoItemKind: String, Codable, Hashable, Sendable {
    case waypoint
    case route
    case note
}

/// One geo item: selection on a map, hit tests, links between features.
struct GeoItemRef: Hashable, Codable, Sendable {
    var kind: GeoItemKind
    var id: UUID
}

/// Where a geo item came from.
enum GeoItemOrigin: Codable, Hashable, Sendable {
    /// Placed in MapMe.
    case user
    /// Imported from a GPX file (its name, e.g. "Bannerman Pass.gpx").
    case gpx(fileName: String)
    /// A waypoint dropped on one map before waypoints were global, moved into the store.
    case migrated(mapID: UUID)
}

/// The glyph of a waypoint.
enum WaypointSymbol: String, Codable, CaseIterable, Identifiable, Sendable {
    case flag
    case pin
    case camp
    case water
    case peak
    case hut
    case parking
    case viewpoint
    case danger
    case start
    case finish

    var id: String { rawValue }

    var systemImage: String {
        switch self {
        case .flag: return "flag.fill"
        case .pin: return "mappin"
        case .camp: return "tent.fill"
        case .water: return "drop.fill"
        case .peak: return "mountain.2.fill"
        case .hut: return "house.fill"
        case .parking: return "parkingsign"
        case .viewpoint: return "binoculars.fill"
        case .danger: return "exclamationmark.triangle.fill"
        case .start: return "figure.walk"
        case .finish: return "flag.checkered"
        }
    }

    var label: String {
        switch self {
        case .flag: return "Flag"
        case .pin: return "Pin"
        case .camp: return "Camp"
        case .water: return "Water"
        case .peak: return "Peak"
        case .hut: return "Hut"
        case .parking: return "Parking"
        case .viewpoint: return "Viewpoint"
        case .danger: return "Danger"
        case .start: return "Start"
        case .finish: return "Finish"
        }
    }
}

/// A named point the user keeps (dropped on a map, or a GPX `<wpt>`).
struct Waypoint: Codable, Hashable, Identifiable, Sendable {
    var id: UUID = UUID()
    var name: String
    var position: LatLon
    /// From the GPX file, or the terrain when it was placed.
    var elevationM: Double? = nil
    var symbol: WaypointSymbol = .flag
    var comment: String? = nil
    var createdAt: Date = Date()
    var origin: GeoItemOrigin = .user
}

/// How a route was made.
enum RouteKind: String, Codable, Hashable, Sendable {
    /// Planned in MapMe by tapping points (anchors), optionally traced freehand along a trail.
    case planned
    /// A GPX `<rte>`: a sequence of route points.
    case gpxRoute
    /// A GPX `<trk>` (someone's recorded track): dense points, edited as a whole.
    case gpxTrack
}

/// The colour a route is drawn in.
enum RouteColor: String, Codable, CaseIterable, Identifiable, Sendable {
    case violet
    case blue
    case teal
    case green
    case orange
    case yellow

    var id: String { rawValue }

    var uiColor: UIColor {
        switch self {
        case .violet: return Theme.uiRoute
        case .blue: return UIColor(red: 0.04, green: 0.52, blue: 1.0, alpha: 1)
        case .teal: return Theme.uiImportedRoute
        case .green: return UIColor(red: 0.20, green: 0.80, blue: 0.36, alpha: 1)
        case .orange: return UIColor(red: 1.0, green: 0.58, blue: 0.0, alpha: 1)
        case .yellow: return UIColor(red: 1.0, green: 0.84, blue: 0.04, alpha: 1)
        }
    }

    var color: Color { Color(uiColor: uiColor) }
}

/// One point of a route.
struct RoutePoint: Codable, Hashable, Sendable {
    var lat: Double
    var lon: Double
    /// Elevation from the file (GPX `<ele>`); statistics and profiles use the terrain.
    var ele: Double? = nil
    /// A point the user placed (a handle while editing) or a GPX `<rtept>`. Points between anchors
    /// (a freehand trace, `<trkpt>`s) are false.
    var isAnchor: Bool
    /// A `<rtept>`'s name, or one the user gave an anchor ("Bannerman Hut").
    var name: String? = nil

    var latLon: LatLon { LatLon(lat: lat, lon: lon) }

    init(lat: Double, lon: Double, ele: Double? = nil, isAnchor: Bool, name: String? = nil) {
        self.lat = lat
        self.lon = lon
        self.ele = ele
        self.isAnchor = isAnchor
        self.name = name
    }

    init(_ p: LatLon, isAnchor: Bool, name: String? = nil) {
        self.init(lat: p.lat, lon: p.lon, isAnchor: isAnchor, name: name)
    }
}

/// A route's distance and climbs from the terrain (`mm_terrain_profile`), cached with the route.
struct RouteStats: Codable, Hashable, Sendable {
    var distanceM: Double
    /// Nil without terrain (offline, no tiles): the distance alone is known.
    var ascentM: Double? = nil
    var descentM: Double? = nil
    var minElevationM: Double? = nil
    var maxElevationM: Double? = nil
    /// DEM zoom the elevations came from.
    var demZoom: Int? = nil
    /// `Route.geometryHash` of the points these stats are for (stale when it differs).
    var geometryHash: Int
}

/// A line to walk: planned in MapMe, or imported from GPX (Gaia, AllTrails, friends).
struct Route: Codable, Hashable, Identifiable, Sendable {
    var id: UUID = UUID()
    var name: String
    var kind: RouteKind
    var points: [RoutePoint]
    var createdAt: Date = Date()
    var updatedAt: Date = Date()
    var origin: GeoItemOrigin = .user
    /// Nil: by kind (planned violet, imported teal).
    var color: RouteColor? = nil
    var comment: String? = nil
    var stats: RouteStats? = nil

    /// The points the user placed (or GPX route points).
    var anchors: [RoutePoint] { points.filter(\.isAnchor) }

    var latLons: [LatLon] { points.map(\.latLon) }

    var displayColor: RouteColor { color ?? (kind == .planned ? .violet : .teal) }

    /// Stable across launches (FNV-1a over the coordinates' bits), unlike `hashValue`: says whether
    /// cached `stats` still fit the points.
    var geometryHash: Int {
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for p in points {
            for bits in [p.lat.bitPattern, p.lon.bitPattern] {
                var b = bits
                for _ in 0..<8 {
                    h ^= b & 0xff
                    h = h &* 0x0000_0100_0000_01b3
                    b >>= 8
                }
            }
        }
        return Int(truncatingIfNeeded: h)
    }

    /// The box around the points (nil for none). O(points): cache it when querying often.
    var bounds: GeoBounds? { GeoBounds.enclosing(latLons) }

    /// Stats that belong to the current points.
    var currentStats: RouteStats? {
        guard let stats, stats.geometryHash == geometryHash else { return nil }
        return stats
    }
}

/// How a note's photo got its position.
enum PhotoPlacement: String, Codable, Hashable, Sendable {
    /// The photo's own GPS position (EXIF).
    case exif
    /// Where you were when it was taken or added.
    case myLocation
    /// Placed by hand (a dropped pin, the map's centre).
    case manual
}

/// A photo attached to a note. The image lives in the store's `photos/` folder, re-encoded
/// without metadata (nothing leaves the device; the position is the note's).
struct NotePhoto: Codable, Hashable, Identifiable, Sendable {
    var id: UUID = UUID()
    /// "<id>.jpg" in `GeoItemStore.photosDirectory`; the thumbnail is "<id>-thumb.jpg".
    var fileName: String
    var pixelWidth: Int
    var pixelHeight: Int
    /// When it was taken (EXIF), if known.
    var takenAt: Date? = nil
    /// Compass direction the camera faced (EXIF GPSImgDirection), degrees.
    var headingDegrees: Double? = nil
    var placement: PhotoPlacement

    var thumbnailFileName: String { "\(id.uuidString)-thumb.jpg" }
}

/// A note pinned on Earth, with an optional photo.
struct MapNote: Codable, Hashable, Identifiable, Sendable {
    var id: UUID = UUID()
    var position: LatLon
    var elevationM: Double? = nil
    var text: String
    var createdAt: Date = Date()
    var updatedAt: Date = Date()
    var photo: NotePhoto? = nil

    /// The first line, for titles.
    var title: String {
        let line = text.split(separator: "\n", omittingEmptySubsequences: true).first.map(String.init) ?? ""
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if !trimmed.isEmpty { return trimmed }
        return photo == nil ? "Note" : "Photo"
    }
}
