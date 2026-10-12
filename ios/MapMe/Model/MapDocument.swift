import CoreGraphics
import Foundation

enum MapSourceKind: String, Codable, Hashable {
    case pdf
    case image
    /// Photo of a paper map: flattened from its page corners, or a document-camera scan.
    case photo
}

/// The original photo of a paper map and the page corners its base raster was flattened from.
struct PhotoSource: Codable, Hashable {
    /// Original photo inside the map folder ("source-photo.heic"); may equal the base raster's
    /// file when the photo was used as is.
    var fileName: String
    /// Size of the upright photo (EXIF orientation applied, long side ≤ 6000 px) the corners refer to.
    var pixelWidth: Int
    var pixelHeight: Int
    /// Page corners TL, TR, BR, BL as [x, y] in those pixels (top-left origin).
    var quad: [[Double]]
    /// 35 mm-equivalent focal length from the photo's EXIF, when known (helps recover the page's aspect ratio).
    var focalLength35mm: Double? = nil
}

enum POIKind: String, Codable, Hashable, CaseIterable {
    case place
    case peak
    case spotHeight = "spot_height"
    case water
    case parking
    case shelter
    case viewpoint
    case waypoint
    case other

    var symbolName: String {
        switch self {
        case .place: return "mappin"
        case .peak, .spotHeight: return "mountain.2.fill"
        case .water: return "drop.fill"
        case .parking: return "parkingsign"
        case .shelter: return "house.fill"
        case .viewpoint: return "binoculars.fill"
        case .waypoint: return "flag.fill"
        case .other: return "circle.fill"
        }
    }
}

enum POISource: String, Codable, Hashable {
    case mapLabel        // read off the map by OCR
    case openStreetMap   // fetched from OSM Overpass
    case user            // dropped by the user
}

struct MapPOI: Codable, Hashable, Identifiable {
    var id: UUID = UUID()
    var name: String
    var kind: POIKind
    var source: POISource
    var position: LatLon
    var elevationM: Double?
}

/// Metadata of one imported map, stored as meta.json in its folder.
struct MapDocument: Codable, Hashable, Identifiable {
    var id: UUID
    var name: String
    var createdAt: Date
    var lastOpenedAt: Date?
    /// File name of the original inside the map folder ("source.pdf", "source.jpg", …).
    var sourceFileName: String
    var sourceKind: MapSourceKind
    /// Base raster size (tile level 0).
    var pixelWidth: Int
    var pixelHeight: Int
    /// Pixels per inch of the base raster when known (PDF renders).
    var renderDPI: Double?
    /// Tile edge in pixels and number of pyramid levels (level 0 = full resolution).
    var tileSize: Int
    var levels: Int
    var georef: Georeference?
    var georefMessage: String?
    var pois: [MapPOI] = []
    /// User-picked contour colour override (RGB 0-255), used when re-locating.
    var contourRGB: [Int]?
    /// Photos of paper maps: the original photo and the page corners (nil for PDFs, images and scans).
    /// Optional, so meta.json files written before photo support still decode.
    var photoSource: PhotoSource? = nil
    /// The last automatic locate when it did not find the map (nil once located): how far it
    /// searched, why it failed and the names on the map, for "Search wider". Optional, so older
    /// meta.json files still decode.
    var locateAttempt: LocateAttempt? = nil

    var pixelSize: CGSize { CGSize(width: pixelWidth, height: pixelHeight) }
    var isGeoreferenced: Bool { georef != nil }
    /// Taken with a camera: georeferenced in photo mode (lighting normalised, projective fit).
    var isPhoto: Bool { sourceKind == .photo }
    /// The page corners can be adjusted again (the original photo is kept).
    var canAdjustCorners: Bool { photoSource != nil }

    /// Centre of the map in geographic coordinates, if georeferenced.
    var center: LatLon? {
        georef?.latLon(forPixel: CGPoint(x: Double(pixelWidth) / 2, y: Double(pixelHeight) / 2))
    }

    /// True when the coordinate falls inside the raster.
    func contains(_ p: LatLon) -> Bool {
        guard let g = georef else { return false }
        let px = g.pixel(for: p)
        return px.x >= 0 && px.y >= 0 && px.x <= CGFloat(pixelWidth) && px.y <= CGFloat(pixelHeight)
    }
}
