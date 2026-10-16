import CoreGraphics
import Foundation
import MapMeCoreFFI

/// Why the core could not plan an area.
struct OfflinePlanFailure: Error, Equatable, Sendable {
    var message: String
}

/// The numbers behind offline terrain (docs/design/explorer.md, Offline regions): the core's tile
/// plan (`mm_tile_plan`), the order tiles download in, which tiles to measure, the measured
/// estimate and the limits. Pure functions, any thread; `plan` is a quick core call.
enum OfflinePlanner {
    /// A plan larger than this is refused (about 15 minutes on good Wi-Fi).
    static let maxTiles = 60_000
    /// An estimate larger than this is refused.
    static let maxBytes = 2.5e9
    /// Tiles whose real sizes are measured before a download (spread over the two finest zooms).
    static let sampleCount = 16
    /// Fewer measured tiles than this and the estimate stays the planner's.
    static let minimumSamples = 4
    /// From this size the picker recommends Wi-Fi.
    static let wifiRecommendedBytes = 25e6
    /// Free space must exceed the estimate × this + `freeSpaceReserveBytes`.
    static let freeSpaceFactor = 1.25
    static let freeSpaceReserveBytes = 500e6

    // MARK: - Plans

    /// The core's plan for `request` (tiles per zoom as disjoint ranges, estimated bytes), or why
    /// it could not make one.
    static func plan(_ request: TilePlanRequest) -> Result<TilePlan, OfflinePlanFailure> {
        guard let body = try? JSONEncoder().encode(request) else {
            return .failure(OfflinePlanFailure(message: "Could not encode the request"))
        }
        let json = String(decoding: body, as: UTF8.self)
        let out: String = json.withCString { cstr -> String in
            guard let res = mm_tile_plan(cstr) else { return "" }
            defer { mm_string_free(res) }
            return String(cString: res)
        }
        let answer: TerrainAnswer<TilePlan> = TerrainStore.decode(out)
        switch answer.status {
        case .ok, .needDEM:
            guard let plan = answer.value else {
                return .failure(OfflinePlanFailure(message: "The terrain engine returned no plan"))
            }
            return .success(plan)
        case .failed(let message):
            return .failure(OfflinePlanFailure(message: message))
        }
    }

    /// An offline region: the box at the detail's zooms.
    static func regionRequest(_ bounds: GeoBounds, detail: OfflineDetail) -> TilePlanRequest {
        TilePlanRequest(areas: [.box(bounds, zooms: detail.zooms)])
    }

    /// The terrain kept around a located map (the core's trailhead rule); nil when the map is not
    /// located.
    static func trailheadRequest(for map: MapDocument) -> TilePlanRequest? {
        corners(of: map).map { TilePlanRequest.trailhead(corners: $0) }
    }

    /// A located map's corners TL, TR, BR, BL (through the warp, as the map is drawn).
    static func corners(of map: MapDocument) -> [LatLon]? {
        guard let g = map.georef, map.pixelWidth > 0, map.pixelHeight > 0 else { return nil }
        let w = CGFloat(map.pixelWidth)
        let h = CGFloat(map.pixelHeight)
        let corners = [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)]
            .map { g.latLon(forPixel: $0) }
        guard corners.allSatisfy({ $0.lat.isFinite && $0.lon.isFinite && abs($0.lat) <= 90 }) else { return nil }
        return corners
    }

    // MARK: - Tiles

    /// The plan's tiles in download order: coarse zooms first (a region half downloaded already
    /// locates maps on its coarse zooms), row by row within a zoom.
    static func orderedTiles(_ plan: TilePlan) -> [TileID] {
        var seen = Set<TileID>()
        var out: [TileID] = []
        out.reserveCapacity(plan.tiles)
        for zoom in plan.zooms.sorted(by: { $0.z < $1.z }) {
            for range in zoom.ranges {
                for t in range.tiles where seen.insert(t).inserted {
                    out.append(t)
                }
            }
        }
        return out
    }

    /// Up to `count` tiles spread evenly over the plan's two finest zooms (half each), the ones
    /// whose sizes decide the estimate.
    static func sampleTiles(_ plan: TilePlan, count: Int = sampleCount) -> [TileID] {
        let finest = Array(plan.zooms.filter { $0.tiles > 0 }.sorted { $0.z > $1.z }.prefix(2))
        guard !finest.isEmpty, count > 0 else { return [] }
        let perZoom = max(1, count / finest.count)
        var out: [TileID] = []
        for zoom in finest {
            let tiles = zoom.ranges.flatMap(\.tiles)
            guard !tiles.isEmpty else { continue }
            let n = min(perZoom, tiles.count)
            // The middle of each of n equal stretches of the row-by-row list: spread across rows.
            for k in 0..<n {
                let i = Int((Double(k) + 0.5) * Double(tiles.count) / Double(n))
                out.append(tiles[min(i, tiles.count - 1)])
            }
        }
        return out
    }

    /// Bytes to download, measured: each sampled zoom's tiles × its sampled mean; the other
    /// (coarser) zooms' table estimate scaled by how far the samples are from the table, within
    /// half the SRTM size and 1.5× the high-resolution size. Nil with fewer than
    /// `minimumSamples` sizes.
    static func measuredBytes(_ plan: TilePlan, samples: [TileID: Int64]) -> Double? {
        var sizes: [Int: [Double]] = [:]
        for (t, bytes) in samples where bytes > 0 {
            sizes[t.z, default: []].append(Double(bytes))
        }
        guard sizes.values.reduce(0, { $0 + $1.count }) >= minimumSamples else { return nil }
        var measured = 0.0
        var table = 0.0
        var sampledZooms = Set<Int>()
        for zoom in plan.zooms {
            guard let s = sizes[zoom.z], !s.isEmpty else { continue }
            let mean = s.reduce(0, +) / Double(s.count)
            measured += Double(zoom.tiles) * mean
            table += zoom.bytes
            sampledZooms.insert(zoom.z)
        }
        guard !sampledZooms.isEmpty else { return nil }
        let ratio = table > 0 ? measured / table : 1
        var total = measured
        for zoom in plan.zooms where !sampledZooms.contains(zoom.z) {
            let low = zoom.bytes * 0.5
            let high = max(zoom.bytes, zoom.bytesHigh) * 1.5
            total += min(max(zoom.bytes * ratio, low), high)
        }
        return total
    }

    /// Why an area cannot be downloaded (nil: it can): over 60 000 tiles or 2.5 GB, or not
    /// enough free space (the estimate × 1.25 + 500 MB).
    static func refusal(tiles: Int, bytes: Double, freeBytes: Int64?, detail: OfflineDetail) -> String? {
        if tiles > maxTiles || bytes > maxBytes {
            return detail == .high
                ? "Too large for High detail: pick a smaller area or Standard"
                : "Too large: pick a smaller area"
        }
        if let freeBytes {
            let needed = bytes * freeSpaceFactor + freeSpaceReserveBytes
            if Double(freeBytes) < needed {
                return "Not enough free space: \(OfflineText.size(bytes: needed)) needed, \(OfflineText.size(bytes: Double(freeBytes))) free"
            }
        }
        return nil
    }

    /// Space left for the user's data on the device (nil when unknown).
    static func freeBytes() -> Int64? {
        let home = URL(fileURLWithPath: NSHomeDirectory())
        guard let values = try? home.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let free = values.volumeAvailableCapacityForImportantUsage
        else { return nil }
        return free
    }
}

/// Wording of offline terrain sizes and counts.
enum OfflineText {
    /// "< 1 MB", "3.4 MB", "342 MB", "1.2 GB" (decimal, like Settings).
    static func size(bytes: Double) -> String {
        let mb = max(bytes, 0) / 1_000_000
        if mb < 1 { return "< 1 MB" }
        if mb < 10 { return String(format: "%.1f MB", mb) }
        if mb < 1_000 { return "\(Int(mb.rounded())) MB" }
        return String(format: "%.1f GB", mb / 1_000)
    }

    static func size(bytes: Int64) -> String { size(bytes: Double(bytes)) }

    /// "2 475 tiles", "1 tile".
    static func tiles(_ n: Int) -> String {
        n == 1 ? "1 tile" : "\(SearchText.grouped(n)) tiles"
    }

    /// "≈ 342 MB · 2 475 tiles · measured".
    static func estimateLine(_ estimate: OfflineEstimate) -> String {
        let how = estimate.isMeasured ? "measured" : "estimated"
        return "\u{2248} \(size(bytes: estimate.bytes)) \u{00B7} \(tiles(estimate.plan.tiles)) \u{00B7} \(how)"
    }
}
