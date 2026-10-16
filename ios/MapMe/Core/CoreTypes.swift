import Foundation

// Codable mirrors of the mapme-core JSON contract (docs/ARCHITECTURE.md).
// Keys are snake_case on the wire; these types spell them out explicitly.

struct OCRObservation: Codable, Hashable {
    var text: String
    /// [x, y, w, h] in base-image pixels, top-left origin.
    var bbox: [Double]
    var confidence: Float
}

struct GPSFix: Codable, Hashable {
    var lat: Double
    var lon: Double
    var accuracyM: Double?

    enum CodingKeys: String, CodingKey {
        case lat, lon
        case accuracyM = "accuracy_m"
    }
}

struct GeorefRequest: Codable {
    var gps: GPSFix?
    var ocr: [OCRObservation] = []
    var dpi: Double?
    var contourRgb: [Int]?
    /// How far from the GPS fix the map may be (the core accepts 1 000 … 2 000 000 m). Up to
    /// 50 km the near search covers it all at full resolution; beyond, the core searches outwards
    /// in chunks at a coarser zoom ("Search wider").
    var searchRadiusM: Double = 50_000
    var allowRotation: Bool = false
    var methods: [String] = ["grid", "contours"]
    var verify: Bool = false
    /// The raster is a (flattened) photo of a paper map: the core normalises uneven lighting and
    /// white balance before reading contours, and fits a full projective transform (homography) so
    /// residual perspective from imperfect page corners is absorbed.
    var photo: Bool = false
    /// Candidate areas searched first at full resolution (places named on the map, found with
    /// Apple Maps). With a GPS fix, hints farther than `searchRadiusM` from it are ignored.
    var hints: [SearchHint] = []
    /// The most tiles one `need_dem` answer may list (the core's default is 64).
    var maxTilesPerRound: Int?

    enum CodingKeys: String, CodingKey {
        case gps, ocr, dpi, methods, verify, photo, hints
        case contourRgb = "contour_rgb"
        case searchRadiusM = "search_radius_m"
        case allowRotation = "allow_rotation"
        case maxTilesPerRound = "max_tiles_per_round"
    }
}

/// A candidate area for the contour search (`"hints"` of the request): a circle around a place.
struct SearchHint: Codable, Hashable, Sendable {
    /// Radius the core searches around a hint unless told otherwise.
    static let defaultRadiusM = 15_000.0

    enum Source: String, Codable, Sendable {
        /// A name on the map found with Apple Maps.
        case place
        /// A coordinate label the core read (it adds these itself).
        case label
        /// Picked by the user.
        case user
    }

    var lat: Double
    var lon: Double
    var radiusM: Double = SearchHint.defaultRadiusM
    var source: Source
    var name: String?

    enum CodingKeys: String, CodingKey {
        case lat, lon, source, name
        case radiusM = "radius_m"
    }

    var center: LatLon { LatLon(lat: lat, lon: lon) }
}

struct TileID: Codable, Hashable, Sendable {
    var z: Int
    var x: Int
    var y: Int
}

struct CorePOI: Codable, Hashable {
    var name: String
    /// "place" | "peak" | "spot_height" | "water"
    var kind: String
    var elevationM: Double?
    var x: Double
    var y: Double

    enum CodingKeys: String, CodingKey {
        case name, kind, x, y
        case elevationM = "elevation_m"
    }
}

struct GeorefResponse: Codable {
    /// "ok" | "need_dem" | "failed"
    var status: String
    /// "grid" | "labels" | "contours" | nil
    var method: String?
    var pixelToMercator: [Double]?
    var confidence: Double?
    var rmsM: Double?
    var scaleMPerPx: Double?
    var rotationDeg: Double?
    var corners: [[Double]]?
    var needTiles: [TileID]?
    var pois: [CorePOI]?
    var message: String?
    var timingsMs: [String: Double]?
    /// Photo mode: correction for paper that does not lie flat; a sheet of more than 40 km: the bend
    /// of its projection (absent or null otherwise). The full model is `pixelToMercator` plus this
    /// warp; see `MeshWarp`.
    var warp: MeshWarp?
    /// Names on the map worth looking up (title, places, peaks, water), best first; present
    /// whenever the request carried OCR text.
    var hintQueries: [String]?
    /// How far the contour search has got (progress UI, failure messages).
    var search: SearchStatus?

    enum CodingKeys: String, CodingKey {
        case status, method, confidence, corners, pois, message, warp, search
        case pixelToMercator = "pixel_to_mercator"
        case rmsM = "rms_m"
        case scaleMPerPx = "scale_m_per_px"
        case rotationDeg = "rotation_deg"
        case needTiles = "need_tiles"
        case timingsMs = "timings_ms"
        case hintQueries = "hint_queries"
    }

    var isOK: Bool { status == "ok" }
    var needsDEM: Bool { status == "need_dem" }

    /// The georeference, when status is ok.
    var georeference: Georeference? {
        guard isOK, let m = pixelToMercator, m.count == 9 else { return nil }
        let method: GeorefMethod
        switch self.method {
        case "grid": method = .grid
        case "labels": method = .labels
        case "contours": method = .contours
        default: method = .manual
        }
        // A malformed warp is dropped: the homography alone is still the best flat fit.
        let usableWarp = warp.flatMap { $0.isValid ? $0 : nil }
        return Georeference(pixelToMercator: Homography(m), method: method, confidence: confidence ?? 0, rmsMeters: rmsM,
                            warp: usableWarp)
    }
}

extension GeorefResponse {
    /// Like the synthesized decoder, except that a `warp` the app cannot read is dropped instead of
    /// failing the whole response (the flat homography is still a usable answer).
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decode(String.self, forKey: .status)
        method = try c.decodeIfPresent(String.self, forKey: .method)
        pixelToMercator = try c.decodeIfPresent([Double].self, forKey: .pixelToMercator)
        confidence = try c.decodeIfPresent(Double.self, forKey: .confidence)
        rmsM = try c.decodeIfPresent(Double.self, forKey: .rmsM)
        scaleMPerPx = try c.decodeIfPresent(Double.self, forKey: .scaleMPerPx)
        rotationDeg = try c.decodeIfPresent(Double.self, forKey: .rotationDeg)
        corners = try c.decodeIfPresent([[Double]].self, forKey: .corners)
        needTiles = try c.decodeIfPresent([TileID].self, forKey: .needTiles)
        pois = try c.decodeIfPresent([CorePOI].self, forKey: .pois)
        message = try c.decodeIfPresent(String.self, forKey: .message)
        timingsMs = try c.decodeIfPresent([String: Double].self, forKey: .timingsMs)
        warp = try? c.decodeIfPresent(MeshWarp.self, forKey: .warp)
        // Progress details: never worth failing the whole response over.
        hintQueries = (try? c.decodeIfPresent([String].self, forKey: .hintQueries)) ?? nil
        search = (try? c.decodeIfPresent(SearchStatus.self, forKey: .search)) ?? nil
    }
}

/// The response's `"search"` object: how far the contour search has got. Every field is optional
/// and read leniently (a count may arrive as 2410 or 2410.0).
struct SearchStatus: Codable, Equatable, Sendable {
    enum Phase: Equatable, Sendable {
        /// Checking candidate areas (places named on the map, coordinate labels).
        case hints
        /// Within 50 km of the fix, at full resolution.
        case near
        /// Beyond 50 km, chunk by chunk outwards.
        case wide
        /// Refining a candidate.
        case refine
        case done
        case other
    }

    var radiusM: Double?
    var searchedRadiusM: Double?
    var coarseZoom: Int?
    var tilesPlanned: Int?
    var tilesFetched: Int?
    var hintsTried: Int?
    /// "hints" | "near" | "wide" | "refine" | "done"
    var stage: String?

    enum CodingKeys: String, CodingKey {
        case stage
        case radiusM = "radius_m"
        case searchedRadiusM = "searched_radius_m"
        case coarseZoom = "coarse_zoom"
        case tilesPlanned = "tiles_planned"
        case tilesFetched = "tiles_fetched"
        case hintsTried = "hints_tried"
    }

    init(radiusM: Double? = nil, searchedRadiusM: Double? = nil, coarseZoom: Int? = nil, tilesPlanned: Int? = nil,
         tilesFetched: Int? = nil, hintsTried: Int? = nil, stage: String? = nil) {
        self.radiusM = radiusM
        self.searchedRadiusM = searchedRadiusM
        self.coarseZoom = coarseZoom
        self.tilesPlanned = tilesPlanned
        self.tilesFetched = tilesFetched
        self.hintsTried = hintsTried
        self.stage = stage
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        radiusM = c.lenientDouble(.radiusM)
        searchedRadiusM = c.lenientDouble(.searchedRadiusM)
        coarseZoom = c.lenientInt(.coarseZoom)
        tilesPlanned = c.lenientInt(.tilesPlanned)
        tilesFetched = c.lenientInt(.tilesFetched)
        hintsTried = c.lenientInt(.hintsTried)
        stage = (try? c.decodeIfPresent(String.self, forKey: .stage)) ?? nil
    }

    var phase: Phase {
        switch stage?.lowercased() {
        case "hints": return .hints
        case "near": return .near
        case "wide": return .wide
        case "refine": return .refine
        case "done": return .done
        default: return .other
        }
    }

    /// 0…1: terrain tiles fetched of those planned, else the share of the circle's area searched.
    var fraction: Double? {
        if let planned = tilesPlanned, planned > 0 {
            return min(max(Double(tilesFetched ?? 0) / Double(planned), 0), 1)
        }
        if let r = radiusM, r > 0, let s = searchedRadiusM, s.isFinite {
            let f = min(max(s / r, 0), 1)
            return f * f
        }
        return nil
    }

    /// The search moved on since `old`: it reached farther, fetched more terrain, tried more hints
    /// or entered another stage.
    func advanced(since old: SearchStatus?) -> Bool {
        guard let old else { return true }
        return (searchedRadiusM ?? 0) > (old.searchedRadiusM ?? 0)
            || (tilesFetched ?? 0) > (old.tilesFetched ?? 0)
            || (hintsTried ?? 0) > (old.hintsTried ?? 0)
            || stage != old.stage
    }
}

/// `mm_search_estimate` request: what a search of `radius_m` around `gps` (and the hints) costs.
struct SearchEstimateRequest: Encodable {
    var radiusM: Double
    var gps: GPSFix?
    var hints: [SearchHint] = []

    enum CodingKeys: String, CodingKey {
        case gps, hints
        case radiusM = "radius_m"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(radiusM, forKey: .radiusM)
        // An explicit null: without a fix the hints alone define where to search.
        try c.encode(gps, forKey: .gps)
        try c.encode(hints, forKey: .hints)
    }
}

/// `mm_search_estimate` answer: {"status":"ok","coarse_zoom","tiles","approx_bytes","approx_seconds"}.
struct SearchEstimate: Equatable, Sendable {
    var coarseZoom: Int?
    /// Terrain tiles the search may download.
    var tiles: Int
    /// What those tiles weigh, from measured average Terrarium tile sizes.
    var approxBytes: Double
    /// Download plus matching time on an iPhone.
    var approxSeconds: Double
}

extension KeyedDecodingContainer {
    /// A number at `key` as Double (nil when absent, null, not a number or not finite).
    func lenientDouble(_ key: Key) -> Double? {
        if let d = (try? decodeIfPresent(Double.self, forKey: key)) ?? nil, d.isFinite { return d }
        if let i = (try? decodeIfPresent(Int.self, forKey: key)) ?? nil { return Double(i) }
        return nil
    }

    /// A number at `key` as Int (2410.0 reads as 2410; nil when absent or not a number).
    func lenientInt(_ key: Key) -> Int? {
        if let i = (try? decodeIfPresent(Int.self, forKey: key)) ?? nil { return i }
        if let d = lenientDouble(key), d.magnitude < 1e15 { return Int(d.rounded()) }
        return nil
    }
}

struct ControlPointInput: Codable, Hashable {
    var x: Double
    var y: Double
    var lat: Double
    var lon: Double
}

struct ControlPointRequest: Codable {
    var points: [ControlPointInput]
    /// "auto" | "similarity" | "affine" | "homography"
    var model: String = "auto"
}

struct ControlPointResponse: Codable {
    var status: String
    var pixelToMercator: [Double]?
    var rmsM: Double?
    var residualsM: [Double]?
    var model: String?
    var message: String?

    enum CodingKeys: String, CodingKey {
        case status, model, message
        case pixelToMercator = "pixel_to_mercator"
        case rmsM = "rms_m"
        case residualsM = "residuals_m"
    }
}
