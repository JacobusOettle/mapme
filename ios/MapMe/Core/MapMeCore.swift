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
}
