import Foundation
import MapMeCoreFFI

/// The core's terrain store (`MMTerrain`, docs/ARCHITECTURE.md "C ABI", Terrain): decoded
/// Terrarium tiles in memory, at most `maxBytes` of them (the least recently used go first), and
/// the queries the explorer features run on them: elevations, route profiles, 3D height grids and
/// contour rendering. It never downloads: a query lacking tiles answers `.needDEM` with
/// `needTiles`, which `TerrainProvider` loads before asking again.
///
/// `@unchecked Sendable`: the core documents every MMTerrain function safe on any thread,
/// concurrently (CATiledLayer's drawing threads, the 3D builder, route statistics); the handle
/// never changes and is freed only in `deinit`. Queries are blocking: call them off the main
/// thread.
final class TerrainStore: @unchecked Sendable {
    private let handle: OpaquePointer

    /// - Parameter maxBytes: decoded elevations held at most (0: the core's 64 MiB).
    init?(maxBytes: UInt64 = 0) {
        guard let made = mm_terrain_new(maxBytes) else { return nil }
        handle = made
    }

    deinit { mm_terrain_free(handle) }

    /// Hands a Terrarium PNG to the store. False when it could not be decoded (the store then
    /// counts the tile as missing) or the core cannot take it.
    @discardableResult
    func addTile(_ t: TileID, png: Data) -> Bool {
        guard DEMService.isValid(t), t.z <= 15 else { return false }
        return png.withUnsafeBytes { buf -> Bool in
            guard let p = buf.baseAddress?.assumingMemoryBound(to: UInt8.self), buf.count > 0 else { return false }
            return mm_terrain_add_tile(handle, UInt8(t.z), UInt32(t.x), UInt32(t.y), p, buf.count) == 0
        }
    }

    /// The tile cannot be had (offline, not downloadable): queries fall back to coarser zooms.
    func markMissing(_ t: TileID) {
        guard DEMService.isValid(t), t.z <= 15 else { return }
        _ = mm_terrain_mark_missing(handle, UInt8(t.z), UInt32(t.x), UInt32(t.y))
    }

    /// Forget the missing marks (the network is back).
    func clearMissing() {
        mm_terrain_clear_missing(handle)
    }

    func stats() -> TerrainStats? {
        guard let res = mm_terrain_stats(handle) else { return nil }
        defer { mm_string_free(res) }
        let json = String(cString: res)
        guard let data = json.data(using: .utf8),
              let wire = try? JSONDecoder().decode(TerrainStatusWire.self, from: data), wire.status == "ok"
        else { return nil }
        return try? JSONDecoder().decode(TerrainStats.self, from: data)
    }

    /// Elevations (metres) at points; nil where unknown.
    func elevations(_ request: ElevationsRequest) -> TerrainAnswer<ElevationsResult> {
        call(request) { handle, cstr in mm_terrain_elevations(handle, cstr) }
    }

    /// Distance, climb, descent and the elevation profile along a polyline.
    func profile(_ request: ProfileRequest) -> TerrainAnswer<ElevationProfile> {
        call(request) { handle, cstr in mm_terrain_profile(handle, cstr) }
    }

    /// Heights on a `cols` × `rows` grid over `request.rect` (2…4096 nodes a side).
    func grid(_ request: GridRequest, cols: Int, rows: Int) -> TerrainAnswer<HeightGrid> {
        guard (2...4096).contains(cols), (2...4096).contains(rows) else { return .failed("Invalid grid \(cols)×\(rows)") }
        guard let body = try? JSONEncoder().encode(request) else { return .failed("Could not encode the request") }
        let json = String(decoding: body, as: UTF8.self)
        var heights = [Float](repeating: .nan, count: cols * rows)
        let handle = self.handle
        let out: String = heights.withUnsafeMutableBufferPointer { buffer -> String in
            json.withCString { cstr -> String in
                guard let res = mm_terrain_grid(handle, cstr, buffer.baseAddress, buffer.count, UInt32(cols), UInt32(rows))
                else { return "" }
                defer { mm_string_free(res) }
                return String(cString: res)
            }
        }
        let wireAnswer: TerrainAnswer<GridResultWire> = Self.decode(out)
        guard let wire = wireAnswer.value else {
            return TerrainAnswer(status: wireAnswer.status, value: nil, needTiles: wireAnswer.needTiles)
        }
        let grid = HeightGrid(rect: request.rect, cols: cols, rows: rows, heights: heights, zoom: wire.zoom,
                              minM: wire.minM, maxM: wire.maxM, missingFraction: wire.missingFraction ?? 0)
        return TerrainAnswer(status: wireAnswer.status, value: grid, needTiles: wireAnswer.needTiles)
    }

    /// Draws contours (and hillshade) into `buffer`: `width` × `height` 8-bit RGBA, premultiplied
    /// alpha, rows `bytesPerRow` apart (every pixel is written). Use with a CGContext of
    /// `CGImageAlphaInfo.premultipliedLast` in sRGB.
    func render(_ request: ContourRenderRequest, into buffer: UnsafeMutableRawBufferPointer, width: Int, height: Int,
                bytesPerRow: Int) -> TerrainAnswer<ContourRenderInfo> {
        guard let base = buffer.baseAddress?.assumingMemoryBound(to: UInt8.self),
              width > 0, height > 0, bytesPerRow >= width * 4, buffer.count >= bytesPerRow * height,
              width <= Int(UInt32.max), height <= Int(UInt32.max), bytesPerRow <= Int(UInt32.max)
        else { return .failed("Invalid buffer") }
        return call(request) { handle, cstr in
            mm_terrain_render(handle, cstr, base, UInt32(width), UInt32(height), UInt32(bytesPerRow))
        }
    }

    // MARK: - Plumbing

    /// Encodes `request`, runs `function` and decodes its answer.
    private func call<Request: Encodable, Value: Decodable>(
        _ request: Request,
        _ function: (OpaquePointer, UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
    ) -> TerrainAnswer<Value> {
        guard let body = try? JSONEncoder().encode(request) else { return .failed("Could not encode the request") }
        let json = String(decoding: body, as: UTF8.self)
        let handle = self.handle
        let out: String = json.withCString { cstr -> String in
            guard let res = function(handle, cstr) else { return "" }
            defer { mm_string_free(res) }
            return String(cString: res)
        }
        return Self.decode(out)
    }

    /// A terrain answer from the core's JSON: status, need_tiles and (unless failed) the value.
    static func decode<Value: Decodable>(_ json: String) -> TerrainAnswer<Value> {
        guard let data = json.data(using: .utf8), !data.isEmpty,
              let wire = try? JSONDecoder().decode(TerrainStatusWire.self, from: data)
        else { return .failed("The terrain engine returned an unexpected response") }
        let tiles = wire.needTiles ?? []
        switch wire.status {
        case "ok", "need_dem":
            let status: TerrainAnswer<Value>.Status = wire.status == "ok" ? .ok : .needDEM
            guard let value = try? JSONDecoder().decode(Value.self, from: data) else {
                return TerrainAnswer(status: .failed("The terrain engine's answer could not be read"), value: nil,
                                     needTiles: tiles)
            }
            return TerrainAnswer(status: status, value: value, needTiles: tiles)
        default:
            return TerrainAnswer(status: .failed(wire.message ?? "failed"), value: nil, needTiles: tiles)
        }
    }
}

extension MapMeCore {
    /// The tiles covering a plan's areas and roughly what they weigh (`mm_tile_plan`, stateless,
    /// quick). Nil when the core cannot say (invalid request, not implemented).
    static func tilePlan(_ request: TilePlanRequest) -> TilePlan? {
        guard let body = try? JSONEncoder().encode(request) else { return nil }
        let json = String(decoding: body, as: UTF8.self)
        let out: String = json.withCString { cstr -> String in
            guard let res = mm_tile_plan(cstr) else { return "" }
            defer { mm_string_free(res) }
            return String(cString: res)
        }
        let answer: TerrainAnswer<TilePlan> = TerrainStore.decode(out)
        return answer.isComplete ? answer.value : nil
    }
}
