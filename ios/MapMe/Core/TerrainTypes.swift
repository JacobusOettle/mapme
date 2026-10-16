import Foundation

// Codable mirrors of the terrain part of the mapme-core JSON contract (docs/ARCHITECTURE.md,
// "C ABI", Terrain; core/src/terrain.rs). Requests encode snake_case keys; nil fields are left
// out (the core's defaults apply).

/// How a terrain query ended (`TerrainAnswer.Status`). Declared outside the generic answer so
/// that answers of different value types share it: a status nested in `TerrainAnswer<Value>`
/// would be a different type for every `Value` and could not be handed from one to another.
enum TerrainQueryStatus: Equatable, Sendable {
    /// Complete: nothing more can be had.
    case ok
    /// Add (or mark missing) `needTiles` and ask again.
    case needDEM
    case failed(String)
}

/// The answer of a terrain query: its status, the value (also with `.needDEM`: what the store
/// could answer from the tiles it holds) and the tiles to add before asking again.
struct TerrainAnswer<Value> {
    typealias Status = TerrainQueryStatus

    var status: Status
    var value: Value?
    var needTiles: [TileID] = []

    var isComplete: Bool { status == .ok }

    static func failed(_ message: String) -> TerrainAnswer { TerrainAnswer(status: .failed(message), value: nil) }
}

extension TerrainAnswer: Sendable where Value: Sendable {}

/// The fields every terrain answer has.
struct TerrainStatusWire: Decodable {
    var status: String
    var message: String?
    var needTiles: [TileID]?

    enum CodingKeys: String, CodingKey {
        case status, message
        case needTiles = "need_tiles"
    }
}

/// `mm_terrain_stats`.
struct TerrainStats: Decodable, Equatable, Sendable {
    var tiles: Int
    var missing: Int
    var bytes: Int64
    var maxBytes: Int64
    var evicted: Int64

    enum CodingKeys: String, CodingKey {
        case tiles, missing, bytes, evicted
        case maxBytes = "max_bytes"
    }
}

/// `[[lat, lon], ...]` as the terrain requests want points.
private func latLonPairs(_ points: [LatLon]) -> [[Double]] {
    points.map { [$0.lat, $0.lon] }
}

/// `mm_terrain_elevations` request.
struct ElevationsRequest: Encodable, Sendable {
    var points: [LatLon]
    var maxZoom: Int = 13
    var minZoom: Int? = nil
    var maxTiles: Int? = nil

    enum CodingKeys: String, CodingKey {
        case points
        case maxZoom = "max_zoom"
        case minZoom = "min_zoom"
        case maxTiles = "max_tiles"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(latLonPairs(points), forKey: .points)
        try c.encode(maxZoom, forKey: .maxZoom)
        try c.encodeIfPresent(minZoom, forKey: .minZoom)
        try c.encodeIfPresent(maxTiles, forKey: .maxTiles)
    }
}

/// `mm_terrain_elevations` answer: one elevation (metres) per point, nil where unknown.
struct ElevationsResult: Decodable, Sendable {
    var elevationsM: [Double?]
    var zoom: Int?

    enum CodingKeys: String, CodingKey {
        case zoom
        case elevationsM = "elevations_m"
    }
}

/// `mm_terrain_profile` request: statistics and elevation profile along a polyline.
struct ProfileRequest: Encodable, Sendable {
    var points: [LatLon]
    var maxZoom: Int = 13
    var minZoom: Int? = nil
    /// Sampling step along the line; nil: half a DEM cell.
    var spacingM: Double? = nil
    /// Gaussian sigma of the smoothing along the line; nil: one DEM cell.
    var smoothingM: Double? = nil
    /// A climb or descent counts once it exceeds this.
    var hysteresisM: Double = 3
    /// Profile samples returned (evenly spaced).
    var maxSamples: Int = 1000
    var maxTiles: Int? = nil

    enum CodingKeys: String, CodingKey {
        case points
        case maxZoom = "max_zoom"
        case minZoom = "min_zoom"
        case spacingM = "spacing_m"
        case smoothingM = "smoothing_m"
        case hysteresisM = "hysteresis_m"
        case maxSamples = "max_samples"
        case maxTiles = "max_tiles"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(latLonPairs(points), forKey: .points)
        try c.encode(maxZoom, forKey: .maxZoom)
        try c.encodeIfPresent(minZoom, forKey: .minZoom)
        try c.encodeIfPresent(spacingM, forKey: .spacingM)
        try c.encodeIfPresent(smoothingM, forKey: .smoothingM)
        try c.encode(hysteresisM, forKey: .hysteresisM)
        try c.encode(maxSamples, forKey: .maxSamples)
        try c.encodeIfPresent(maxTiles, forKey: .maxTiles)
    }
}

/// One point of an elevation profile: distance along the line and the elevation there.
struct ProfileSample: Codable, Hashable, Sendable {
    var distanceM: Double
    /// Nil where the terrain is unknown.
    var elevationM: Double?

    init(distanceM: Double, elevationM: Double?) {
        self.distanceM = distanceM
        self.elevationM = elevationM
    }

    /// `[distance_m, elevation_m|null]` on the wire.
    init(from decoder: Decoder) throws {
        var c = try decoder.unkeyedContainer()
        distanceM = try c.decode(Double.self)
        elevationM = try c.decodeIfPresent(Double.self)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.unkeyedContainer()
        try c.encode(distanceM)
        if let elevationM { try c.encode(elevationM) } else { try c.encodeNil() }
    }
}

/// `mm_terrain_profile` answer.
struct ElevationProfile: Codable, Hashable, Sendable {
    /// Geodesic length through the points.
    var distanceM: Double
    /// Smoothed climbs and descents (nil without any elevation).
    var ascentM: Double?
    var descentM: Double?
    var minM: Double?
    var maxM: Double?
    var samples: [ProfileSample]
    /// Cumulative distance of each input point (maps the profile to route points and progress).
    var vertexDistanceM: [Double]
    var vertexElevationM: [Double?]
    /// DEM zoom the elevations came from.
    var zoom: Int?
    /// Share of samples without an elevation.
    var missingFraction: Double

    enum CodingKeys: String, CodingKey {
        case samples, zoom
        case distanceM = "distance_m"
        case ascentM = "ascent_m"
        case descentM = "descent_m"
        case minM = "min_m"
        case maxM = "max_m"
        case vertexDistanceM = "vertex_distance_m"
        case vertexElevationM = "vertex_elevation_m"
        case missingFraction = "missing_fraction"
    }
}

/// `mm_terrain_grid` request: heights over a Web Mercator rectangle.
struct GridRequest: Encodable, Sendable {
    var rect: MercatorRect
    var maxZoom: Int = 13
    var minZoom: Int? = nil
    var maxTiles: Int? = nil

    enum CodingKeys: String, CodingKey {
        case rect
        case maxZoom = "max_zoom"
        case minZoom = "min_zoom"
        case maxTiles = "max_tiles"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(rect.array, forKey: .rect)
        try c.encode(maxZoom, forKey: .maxZoom)
        try c.encodeIfPresent(minZoom, forKey: .minZoom)
        try c.encodeIfPresent(maxTiles, forKey: .maxTiles)
    }
}

/// The fields of `mm_terrain_grid`'s answer besides the heights.
struct GridResultWire: Decodable {
    var zoom: Int?
    var minM: Double?
    var maxM: Double?
    var missingFraction: Double?

    enum CodingKeys: String, CodingKey {
        case zoom
        case minM = "min_m"
        case maxM = "max_m"
        case missingFraction = "missing_fraction"
    }
}

/// Heights on a regular grid (metres, not exaggerated): row 0 along `rect.maxY` (north), node
/// (i, j) at (minX + i·width/(cols − 1), maxY − j·height/(rows − 1)); NaN where unknown.
struct HeightGrid: Sendable {
    var rect: MercatorRect
    var cols: Int
    var rows: Int
    var heights: [Float]
    var zoom: Int?
    var minM: Double?
    var maxM: Double?
    var missingFraction: Double

    func height(col: Int, row: Int) -> Float { heights[row * cols + col] }

    /// Mercator position of node (col, row).
    func mercator(col: Int, row: Int) -> SIMD2<Double> {
        SIMD2(rect.minX + Double(col) * rect.width / Double(max(cols - 1, 1)),
              rect.maxY - Double(row) * rect.height / Double(max(rows - 1, 1)))
    }
}

/// `mm_terrain_render` request: contours (and hillshade) for a rectangle of a map's pixels or of
/// Web Mercator.
struct ContourRenderRequest: Encodable, Sendable {
    enum Space: String, Encodable, Sendable {
        /// Base-image pixels of a map, through `pixelToMercator` (+ `warp`).
        case map
        /// Web Mercator metres.
        case mercator
    }

    enum Style: String, Encodable, Sendable {
        case dark
        case paper
    }

    var space: Space
    /// Map: [x0, y0, x1, y1] base-image pixels (y down); mercator: [minX, minY, maxX, maxY].
    var rect: [Double]
    /// Space `.map`: the georeference (`Georeference.pixelToMercator.m`, `.warp`).
    var pixelToMercator: [Double]? = nil
    var warp: MeshWarp? = nil
    var style: Style
    /// Nil: automatic from `scaleMPerPt` (`ContourInterval.automatic`).
    var intervalM: Double? = nil
    /// Ground metres per point the interval and line widths are chosen for: give every tile of a
    /// level the same value so the tiles agree.
    var scaleMPerPt: Double? = nil
    var indexEvery: Int = 5
    /// 0 … 1.
    var hillshade: Double = 0
    /// Buffer pixels per point (the screen scale for CATiledLayer tiles).
    var lineScale: Double = 1
    /// RGBA 0…255 (straight alpha); nil: the style's paper, opaque; [0, 0, 0, 0]: lines only.
    var background: [Int]? = nil
    var maxZoom: Int = 13
    var minZoom: Int? = nil
    var maxTiles: Int? = nil
    /// Index lines labelled with their elevation where they run straight and uncrowded; nil: the
    /// core's default (on).
    var labels: Bool? = nil

    enum CodingKeys: String, CodingKey {
        case space, rect, warp, style, hillshade, background, labels
        case pixelToMercator = "pixel_to_mercator"
        case intervalM = "interval_m"
        case scaleMPerPt = "scale_m_per_pt"
        case indexEvery = "index_every"
        case lineScale = "line_scale"
        case maxZoom = "max_zoom"
        case minZoom = "min_zoom"
        case maxTiles = "max_tiles"
    }
}

/// `mm_terrain_render` answer.
struct ContourRenderInfo: Decodable, Sendable {
    var intervalM: Double?
    var indexIntervalM: Double?
    var zoom: Int?
    var missingFraction: Double?

    enum CodingKeys: String, CodingKey {
        case zoom
        case intervalM = "interval_m"
        case indexIntervalM = "index_interval_m"
        case missingFraction = "missing_fraction"
    }
}

/// The core's automatic contour interval (`terrain::auto_interval_m`), for the app to pick one
/// interval per zoom level of a tiled view.
enum ContourInterval {
    static let choices: [Double] = [5, 10, 20, 25, 50, 100, 200, 250, 500, 1000]

    /// The smallest choice at least 4 × the ground metres per point.
    static func automatic(metersPerPoint: Double) -> Double {
        let want = 4 * metersPerPoint
        guard want.isFinite, want > 0 else { return choices[0] }
        return choices.first { $0 >= want } ?? choices[choices.count - 1]
    }
}

/// Inclusive tile rectangle of one zoom.
struct TileRange: Codable, Hashable, Sendable {
    var z: Int
    var x0: Int
    var x1: Int
    var y0: Int
    var y1: Int

    var count: Int { max(0, x1 - x0 + 1) * max(0, y1 - y0 + 1) }

    func contains(_ t: TileID) -> Bool {
        t.z == z && t.x >= x0 && t.x <= x1 && t.y >= y0 && t.y <= y1
    }

    /// Its tiles, row by row.
    var tiles: [TileID] {
        guard count > 0 else { return [] }
        var out: [TileID] = []
        out.reserveCapacity(count)
        for y in y0...y1 {
            for x in x0...x1 {
                out.append(TileID(z: z, x: x, y: y))
            }
        }
        return out
    }
}

/// `mm_tile_plan` request.
struct TilePlanRequest: Encodable, Sendable {
    /// One area: exactly one of `bbox`, `circle`, `quad`.
    struct Area: Encodable, Sendable {
        var bbox: [Double]? = nil
        var circle: [Double]? = nil
        var quad: [[Double]]? = nil
        var marginM: Double? = nil
        var zooms: [Int]

        enum CodingKeys: String, CodingKey {
            case bbox, circle, quad, zooms
            case marginM = "margin_m"
        }

        static func box(_ b: GeoBounds, zooms: [Int]) -> Area { Area(bbox: b.array, zooms: zooms) }

        static func circle(_ c: LatLon, radiusM: Double, zooms: [Int]) -> Area {
            Area(circle: [c.lat, c.lon, radiusM], zooms: zooms)
        }

        /// A map's corners (TL, TR, BR, BL) grown by `marginM`.
        static func quad(_ corners: [LatLon], marginM: Double, zooms: [Int]) -> Area {
            Area(quad: latLonPairs(corners), marginM: marginM, zooms: zooms)
        }
    }

    struct Trailhead: Encodable, Sendable {
        /// [lat, lon] TL, TR, BR, BL.
        var corners: [[Double]]
    }

    var areas: [Area] = []
    var trailhead: Trailhead? = nil

    /// The terrain kept around a located map (the core's trailhead rule, docs/design/explorer.md).
    static func trailhead(corners: [LatLon]) -> TilePlanRequest {
        TilePlanRequest(areas: [], trailhead: Trailhead(corners: latLonPairs(corners)))
    }
}

/// `mm_tile_plan` answer.
struct TilePlan: Decodable, Hashable, Sendable {
    struct Zoom: Decodable, Hashable, Sendable {
        var z: Int
        var tiles: Int
        var bytes: Double
        var bytesHigh: Double
        var ranges: [TileRange]

        enum CodingKeys: String, CodingKey {
            case z, tiles, bytes, ranges
            case bytesHigh = "bytes_high"
        }

        init(z: Int, tiles: Int, bytes: Double, bytesHigh: Double, ranges: [TileRange]) {
            self.z = z
            self.tiles = tiles
            self.bytes = bytes
            self.bytesHigh = bytesHigh
            self.ranges = ranges
        }

        /// Ranges are `[x0, x1, y0, y1]` on the wire, their zoom the zoom's.
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            z = try c.decode(Int.self, forKey: .z)
            tiles = c.lenientInt(.tiles) ?? 0
            bytes = c.lenientDouble(.bytes) ?? 0
            bytesHigh = c.lenientDouble(.bytesHigh) ?? bytes
            let raw = try c.decodeIfPresent([[Int]].self, forKey: .ranges) ?? []
            let zoom = z
            ranges = raw.compactMap { r in
                r.count == 4 ? TileRange(z: zoom, x0: r[0], x1: r[1], y0: r[2], y1: r[3]) : nil
            }
        }
    }

    struct Dropped: Decodable, Hashable, Sendable {
        var z: Int
        var tiles: Int
        var reason: String
    }

    var tiles: Int
    /// Estimated download, SRTM-only terrain.
    var bytes: Double
    /// Estimated download over high-resolution sources (US, Europe).
    var bytesHigh: Double
    var zooms: [Zoom]
    var dropped: [Dropped]

    enum CodingKeys: String, CodingKey {
        case tiles, bytes, zooms, dropped
        case bytesHigh = "bytes_high"
    }

    init(tiles: Int, bytes: Double, bytesHigh: Double, zooms: [Zoom], dropped: [Dropped]) {
        self.tiles = tiles
        self.bytes = bytes
        self.bytesHigh = bytesHigh
        self.zooms = zooms
        self.dropped = dropped
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        tiles = c.lenientInt(.tiles) ?? 0
        bytes = c.lenientDouble(.bytes) ?? 0
        bytesHigh = c.lenientDouble(.bytesHigh) ?? bytes
        zooms = try c.decodeIfPresent([Zoom].self, forKey: .zooms) ?? []
        dropped = (try? c.decodeIfPresent([Dropped].self, forKey: .dropped)) ?? []
    }

    var ranges: [TileRange] { zooms.flatMap(\.ranges) }
}
