import CoreGraphics
import Foundation
import MapMeCoreFFI

enum MapMeCoreError: Error, LocalizedError {
    case invalidImage
    case badResponse(String)

    var errorDescription: String? {
        switch self {
        case .invalidImage: return "The map image could not be handed to the georeferencing engine."
        case .badResponse(let s): return "Georeferencing engine returned an unexpected response: \(s)"
        }
    }
}

/// Swift wrapper around the Rust georeferencing session (C ABI in mapme_core.h).
/// Not thread-safe: call `georeference` from one background task at a time.
/// `progress` and `cancel()` may be used from any thread.
final class GeorefSession {
    private let handle: OpaquePointer

    /// - Parameters:
    ///   - rgba: 8-bit RGBA pixels (alpha ignored), `bytesPerRow` apart. Copied by the core.
    init(rgba: UnsafeRawBufferPointer, width: Int, height: Int, bytesPerRow: Int) throws {
        guard let base = rgba.baseAddress?.assumingMemoryBound(to: UInt8.self),
              width > 0, height > 0, bytesPerRow >= width * 4, rgba.count >= bytesPerRow * height,
              let h = mm_session_new(base, UInt32(width), UInt32(height), UInt32(bytesPerRow))
        else { throw MapMeCoreError.invalidImage }
        handle = h
    }

    deinit { mm_session_free(handle) }

    @discardableResult
    func addDEMTile(_ t: TileID, png: Data) -> Bool {
        png.withUnsafeBytes { buf -> Bool in
            guard let p = buf.baseAddress?.assumingMemoryBound(to: UInt8.self) else { return false }
            return mm_session_add_dem_tile(handle, UInt8(t.z), UInt32(t.x), UInt32(t.y), p, buf.count) == 0
        }
    }

    func markDEMTileMissing(_ t: TileID) {
        _ = mm_session_mark_dem_tile_missing(handle, UInt8(t.z), UInt32(t.x), UInt32(t.y))
    }

    /// Blocking; run off the main thread.
    func georeference(_ request: GeorefRequest) throws -> GeorefResponse {
        let json = try JSONEncoder().encode(request)
        let str = String(decoding: json, as: UTF8.self)
        let out: String = str.withCString { cstr in
            guard let res = mm_session_georeference(handle, cstr) else { return "" }
            defer { mm_string_free(res) }
            return String(cString: res)
        }
        guard let data = out.data(using: .utf8), !data.isEmpty else { throw MapMeCoreError.badResponse("empty") }
        do {
            return try JSONDecoder().decode(GeorefResponse.self, from: data)
        } catch {
            throw MapMeCoreError.badResponse(String(out.prefix(300)))
        }
    }

    var progress: Float { mm_session_progress(handle) }

    func cancel() { mm_session_cancel(handle) }
}

enum MapMeCore {
    static var version: String {
        guard let p = mm_version() else { return "?" }
        defer { mm_string_free(p) }
        let s = String(cString: p)
        if let d = s.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
           let v = obj["version"] as? String {
            return v
        }
        return s
    }

    static func fitControlPoints(_ request: ControlPointRequest) throws -> ControlPointResponse {
        let json = try JSONEncoder().encode(request)
        let str = String(decoding: json, as: UTF8.self)
        let out: String = str.withCString { cstr in
            guard let res = mm_fit_control_points(cstr) else { return "" }
            defer { mm_string_free(res) }
            return String(cString: res)
        }
        guard let data = out.data(using: .utf8), !data.isEmpty else { throw MapMeCoreError.badResponse("empty") }
        return try JSONDecoder().decode(ControlPointResponse.self, from: data)
    }

    /// Find the sheet of paper (the map page) in a photo with the core's page finder
    /// (`mm_detect_page`). `rgba` holds 8-bit RGBA pixels (alpha ignored), rows `bytesPerRow`
    /// apart; the core copies them. Nil when no page stands out from its surroundings or the
    /// input is invalid. Blocking: call off the main thread.
    static func detectPage(rgba: UnsafeRawBufferPointer, width: Int, height: Int, bytesPerRow: Int) -> PageDetection? {
        guard let base = rgba.baseAddress?.assumingMemoryBound(to: UInt8.self),
              width > 0, height > 0, bytesPerRow >= width * 4, rgba.count >= bytesPerRow * height,
              width <= Int(UInt32.max), height <= Int(UInt32.max), bytesPerRow <= Int(UInt32.max)
        else { return nil }
        guard let res = mm_detect_page(base, UInt32(width), UInt32(height), UInt32(bytesPerRow)) else { return nil }
        defer { mm_string_free(res) }
        let json = String(cString: res)
        return decodePageDetection(json, width: width, height: height)
    }

    /// Parse `mm_detect_page`'s answer for a `width` × `height` raster. Nil for
    /// `{"status":"failed",...}` and for anything malformed (not four finite corners).
    static func decodePageDetection(_ json: String, width: Int, height: Int) -> PageDetection? {
        guard let data = json.data(using: .utf8),
              let wire = try? JSONDecoder().decode(PageDetectionWire.self, from: data),
              wire.status == "ok",
              let corners = wire.quad, corners.count == 4,
              corners.allSatisfy({ $0.count == 2 && $0[0].isFinite && $0[1].isFinite })
        else { return nil }
        let quad = corners.map { CGPoint(x: $0[0], y: $0[1]) }
        var confidence = wire.confidence ?? 0
        if !confidence.isFinite { confidence = 0 }
        confidence = min(max(confidence, 0), 1)
        var coverage = wire.coverage ?? .nan
        if !coverage.isFinite {
            let imageArea = Double(width) * Double(height)
            coverage = imageArea > 0 ? PageDetection.shoelaceArea(quad) / imageArea : 0
        }
        return PageDetection(quad: quad, confidence: confidence, coverage: coverage)
    }
}

/// The sheet of paper the core's page finder located in a photo (`MapMeCore.detectPage`).
struct PageDetection {
    /// Corners TL, TR, BR, BL in pixels of the raster that was searched (top-left origin, y down).
    var quad: [CGPoint]
    /// 0…1: how clearly the page stands out from its surroundings.
    var confidence: Double
    /// Quad area ÷ image area.
    var coverage: Double

    /// Unsigned area of a polygon in pixels².
    static func shoelaceArea(_ points: [CGPoint]) -> Double {
        guard points.count >= 3 else { return 0 }
        var sum = 0.0
        for i in points.indices {
            let a = points[i]
            let b = points[(i + 1) % points.count]
            sum += Double(a.x) * Double(b.y) - Double(b.x) * Double(a.y)
        }
        return abs(sum) / 2
    }
}

/// `mm_detect_page` JSON: {"status":"ok","quad":[[x,y]×4],"confidence","coverage"} or
/// {"status":"failed","message"}.
private struct PageDetectionWire: Decodable {
    var status: String
    var quad: [[Double]]?
    var confidence: Double?
    var coverage: Double?
    var message: String?
}
