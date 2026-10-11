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
    var searchRadiusM: Double = 20_000
    var allowRotation: Bool = false
    var methods: [String] = ["grid", "contours"]
    var verify: Bool = false
    /// The raster is a (flattened) photo of a paper map: the core normalises uneven lighting and
    /// white balance before reading contours, and fits a full projective transform (homography) so
    /// residual perspective from imperfect page corners is absorbed.
    var photo: Bool = false

    enum CodingKeys: String, CodingKey {
        case gps, ocr, dpi, methods, verify, photo
        case contourRgb = "contour_rgb"
        case searchRadiusM = "search_radius_m"
        case allowRotation = "allow_rotation"
    }
}

struct TileID: Codable, Hashable {
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
    /// Photo mode: correction for paper that does not lie flat (absent or null otherwise). The full
    /// model is `pixelToMercator` plus this warp; see `MeshWarp`.
    var warp: MeshWarp?

    enum CodingKeys: String, CodingKey {
        case status, method, confidence, corners, pois, message, warp
        case pixelToMercator = "pixel_to_mercator"
        case rmsM = "rms_m"
        case scaleMPerPx = "scale_m_per_px"
        case rotationDeg = "rotation_deg"
        case needTiles = "need_tiles"
        case timingsMs = "timings_ms"
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
