import CoreGraphics
import Foundation
import Observation
import UIKit

// Building the 3D view (docs/design/explorer.md, 3D view): heights from the core's grid, the
// draped texture (terrain contours and hillshade from the core's render, the sheet's own raster
// on top), the mesh, and the geo items to show on it. Runs off the main thread (~1 s with the
// tiles held); terrain comes through `TerrainProvider`, so it works offline from pinned regions,
// the terrain kept around located maps and the cache, and says so when it cannot.

/// How far the build has got (the loading ring).
struct Terrain3DProgress: Equatable, Sendable {
    enum Step: Equatable, Sendable {
        case terrain
        case contours
        case map
        case mesh

        var label: String {
            switch self {
            case .terrain: return "Loading terrain…"
            case .contours: return "Drawing contours…"
            case .map: return "Draping the map…"
            case .mesh: return "Building the 3D view…"
            }
        }
    }

    /// 0…1.
    var fraction: Double
    var step: Step
    /// Terrain tiles asked for so far.
    var tiles: Int = 0

    static let start = Terrain3DProgress(fraction: 0.02, step: .terrain)
}

/// How much of the block's terrain is known.
enum Terrain3DTerrainState: Equatable, Sendable {
    case complete
    /// Some heights unknown and filled in from around them: the share.
    case partial(Double)
    /// Most or all unknown (offline, nothing on the device): the block is shown flat.
    case missing
    /// The terrain engine answered with an error: shown flat.
    case unavailable(String)

    var isFlat: Bool {
        switch self {
        case .missing, .unavailable: return true
        case .complete, .partial: return false
        }
    }
}

/// Everything the 3D scene is made of. `@unchecked Sendable`: immutable after the build (the
/// CGImage included), handed once from the builder to the main thread.
struct Terrain3DModel: @unchecked Sendable {
    let id = UUID()
    let mapID: UUID
    let area: Terrain3DArea
    let surface: Terrain3DSurface
    let mesh: Terrain3DMesh
    /// Every second node, drawn while the block is small on screen.
    let coarseMesh: Terrain3DMesh?
    /// The enlarged sheet in its pixel space: terrain beyond the sheet, the sheet on it.
    let texture: CGImage
    let terrain: Terrain3DTerrainState
    /// DEM zoom of the heights and the contour interval drawn beyond the sheet.
    let demZoom: Int?
    let contourIntervalM: Double?
    /// Height above sea level of the block's y = 0 (its lowest terrain), metres.
    let baseM: Double
    /// The terrain's range, metres (nil when shown flat).
    let minElevationM: Double?
    let maxElevationM: Double?
    let dark: Bool

    /// The block's size on the ground, metres (its longer side).
    var extentM: Double {
        var minX = Double.infinity
        var maxX = -Double.infinity
        var minZ = Double.infinity
        var maxZ = -Double.infinity
        for n in surface.nodes {
            minX = min(minX, n.x)
            maxX = max(maxX, n.x)
            minZ = min(minZ, n.y)
            maxZ = max(maxZ, n.y)
        }
        let extent = max(maxX - minX, maxZ - minZ)
        return extent.isFinite && extent > 0 ? extent : 10_000
    }
}

enum Terrain3DBuilder {
    /// What the build needs from the map and the settings (taken on the main thread).
    struct Request: Sendable {
        var mapID: UUID
        var georef: Georeference
        var sheetWidth: Int
        var sheetHeight: Int
        var pyramidLevels: Int
        /// Map pixels around the sheet (the continuation margin).
        var margin: Double
        var dark: Bool
        /// Finest terrain zoom (the battery saver's 12, else 13).
        var maxZoom: Int
        /// The texture's long side (by device memory).
        var textureSide: Int
    }

    /// Off-map paper behind the contours (the core's styles, `Theme.uiOffMap*`), used where the
    /// render cannot draw.
    static func offMapPaper(dark: Bool) -> UIColor { dark ? Theme.uiOffMapNightPaper : Theme.uiOffMapPaper }

    /// Builds the model; nil when the georeference does not place the block or memory for the
    /// texture cannot be had. Progress is reported on the main actor. Cancellable.
    static func build(_ request: Request, terrain: TerrainProvider, tileStore: TileStore,
                      progress: @escaping @MainActor @Sendable (Terrain3DProgress) -> Void) async -> Terrain3DModel? {
        guard let area = Terrain3DArea(georef: request.georef, sheetWidth: request.sheetWidth,
                                       sheetHeight: request.sheetHeight, margin: request.margin)
        else { return nil }
        var tiles = 0

        // 1. Heights: the core's grid over the block's Mercator box, at zoom 12 (13 for a small
        // sheet), loading the tiles it asks for (pinned, cached, then the network).
        let zoom = Terrain3DGridRule.meshZoom(nodeSpacingMercator: area.nodeSpacingMercator, maxZoom: request.maxZoom)
        let size = Terrain3DGridRule.coreGridSize(rect: area.mercatorRect, nodeSpacingMercator: area.nodeSpacingMercator)
        let gridRequest = GridRequest(rect: area.mercatorRect, maxZoom: zoom)
        await progress(Terrain3DProgress(fraction: 0.04, step: .terrain, tiles: 0))
        let gridAnswer: TerrainAnswer<HeightGrid>? = await resolve(
            terrain, range: 0.04...0.5, step: .terrain, tiles: &tiles, progress: progress,
            query: { store in store.grid(gridRequest, cols: size.cols, rows: size.rows) },
            missing: { grid in grid.missingFraction })
        guard !Task.isCancelled else { return nil }

        // 2. The texture: contours and hillshade over the whole enlarged sheet, in its pixels.
        let texSize = Terrain3DTextureRule.size(enlargedWidth: area.enlargedWidth, enlargedHeight: area.enlargedHeight,
                                                maxSide: request.textureSide)
        let bytesPerRow = texSize.width * 4
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: texSize.width, height: texSize.height, bitsPerComponent: 8,
                                  bytesPerRow: bytesPerRow, space: space,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let data = ctx.data
        else { return nil }
        // Every pixel gets the off-map paper first: what stays where the render cannot draw.
        ctx.setFillColor(offMapPaper(dark: request.dark).cgColor)
        ctx.fill(CGRect(x: 0, y: 0, width: texSize.width, height: texSize.height))

        let texelGround = area.metersPerPixel * area.enlargedWidth / Double(texSize.width)
        var renderZoom = Terrain3DTextureRule.renderZoom(texelGroundM: texelGround, groundScale: area.groundScale,
                                                         maxZoom: min(request.maxZoom, 13))
        while TileMath.tiles(covering: area.mercatorRect, zoom: renderZoom).count > Terrain3DLimits.maxTextureTiles,
              renderZoom > 8 {
            renderZoom -= 1
        }
        let lineScale = Terrain3DTextureRule.lineScale
        // In horizontal strips: each asks only for its own tiles, and the core draws a rectangle
        // whole or in parts with the same pixels (one interval for all: `scale_m_per_pt`).
        let strips = max(1, min(16, texSize.height / 512))
        var renderInfo: ContourRenderInfo?
        for s in 0..<strips {
            guard !Task.isCancelled else { return nil }
            let y0 = texSize.height * s / strips
            let y1 = texSize.height * (s + 1) / strips
            let top = -area.margin + Double(y0) / Double(texSize.height) * area.enlargedHeight
            let bottom = -area.margin + Double(y1) / Double(texSize.height) * area.enlargedHeight
            let renderRequest = ContourRenderRequest(
                space: .map, rect: [-area.margin, top, area.sheetWidth + area.margin, bottom],
                pixelToMercator: request.georef.pixelToMercator.m, warp: request.georef.warp,
                style: request.dark ? .dark : .paper, intervalM: nil, scaleMPerPt: texelGround * lineScale,
                indexEvery: 5, hillshade: request.dark ? 0.25 : 0.35, lineScale: lineScale, background: nil,
                maxZoom: renderZoom)
            let buffer = UnsafeMutableRawBufferPointer(start: data + y0 * bytesPerRow, count: (y1 - y0) * bytesPerRow)
            let width = texSize.width
            let height = y1 - y0
            let lower = 0.52 + 0.3 * Double(s) / Double(strips)
            let upper = 0.52 + 0.3 * Double(s + 1) / Double(strips)
            let answer: TerrainAnswer<ContourRenderInfo>? = await resolve(
                terrain, range: lower...upper, step: .contours, tiles: &tiles, progress: progress,
                query: { store in store.render(renderRequest, into: buffer, width: width, height: height, bytesPerRow: bytesPerRow) },
                missing: { info in info.missingFraction })
            if renderInfo == nil { renderInfo = answer?.value }
            if answer?.value == nil {
                // Failed (no store, an error): the strip keeps the paper.
                let flipped = CGRect(x: 0, y: texSize.height - y1, width: width, height: height)
                ctx.setFillColor(offMapPaper(dark: request.dark).cgColor)
                ctx.fill(flipped)
            }
            await progress(Terrain3DProgress(fraction: upper, step: .contours, tiles: tiles))
        }
        guard !Task.isCancelled else { return nil }

        // 3. The sheet itself, as on the map screen (dark or as printed), with a hairline edge.
        await progress(Terrain3DProgress(fraction: 0.84, step: .map, tiles: tiles))
        let sheetRect = area.sheetRect(textureWidth: texSize.width, height: texSize.height)
        let longSide = Terrain3DTextureRule.sheetLongSide(sheetTexels: Double(max(sheetRect.width, sheetRect.height)))
        if let sheet = autoreleasepool(invoking: { sheetImage(tileStore: tileStore, request: request, longSide: longSide) }) {
            // CoreGraphics' origin is the bottom-left corner.
            let drawn = CGRect(x: sheetRect.minX, y: CGFloat(texSize.height) - sheetRect.maxY,
                               width: sheetRect.width, height: sheetRect.height)
            ctx.saveGState()
            ctx.interpolationQuality = .high
            ctx.draw(sheet, in: drawn)
            ctx.setStrokeColor((request.dark ? Theme.uiSheetEdge : Theme.uiSheetEdgePaper).cgColor)
            ctx.setLineWidth(1.5)
            ctx.stroke(drawn)
            ctx.restoreGState()
        }
        guard let texture = ctx.makeImage(), !Task.isCancelled else { return nil }

        // 4. The mesh: heights at its nodes, gaps filled (or flat when most is unknown).
        await progress(Terrain3DProgress(fraction: 0.9, step: .mesh, tiles: tiles))
        var heights = Terrain3DHeights.meshHeights(area: area, grid: gridAnswer?.value)
        let unknownCount = heights.reduce(0) { $0 + ($1.isFinite ? 0 : 1) }
        let unknownShare = Double(unknownCount) / Double(max(heights.count, 1))
        let state: Terrain3DTerrainState
        if let gridAnswer {
            if case .failed(let message) = gridAnswer.status {
                state = .unavailable(message)
            } else if unknownShare > Terrain3DLimits.flatAboveMissing {
                state = .missing
            } else if unknownShare > Terrain3DLimits.completeBelowMissing {
                state = .partial(unknownShare)
            } else {
                state = .complete
            }
        } else {
            state = .unavailable("The terrain engine isn't available")
        }
        if state.isFlat {
            heights = [Float](repeating: 0, count: heights.count)
        } else {
            Terrain3DHeights.fillGaps(&heights, cols: area.cols, rows: area.rows)
        }
        let base = heights.min() ?? 0
        let highest = heights.max() ?? 0
        let nodes = area.localNodes()
        let mesh = Terrain3DMesh.build(cols: area.cols, rows: area.rows, nodes: nodes, heights: heights, baseM: base)
        let coarse = Terrain3DMesh.build(cols: area.cols, rows: area.rows, nodes: nodes, heights: heights, baseM: base,
                                         step: Terrain3DLimits.coarseStep)
        let surface = Terrain3DSurface(cols: area.cols, rows: area.rows, nodes: nodes, heights: heights.map { $0 - base })
        await progress(Terrain3DProgress(fraction: 0.98, step: .mesh, tiles: tiles))
        return Terrain3DModel(mapID: request.mapID, area: area, surface: surface, mesh: mesh, coarseMesh: coarse,
                              texture: texture, terrain: state, demZoom: gridAnswer?.value?.zoom,
                              contourIntervalM: renderInfo?.intervalM, baseM: Double(base),
                              minElevationM: state.isFlat ? nil : Double(base),
                              maxElevationM: state.isFlat ? nil : Double(highest), dark: request.dark)
    }

    /// Runs a terrain query until it is complete, loading what it asks for (at most
    /// `TerrainProvider.defaultMaxRounds` rounds), reporting progress within `range` by the share
    /// of the answer that is known. Nil without a terrain store.
    private static func resolve<Value>(_ terrain: TerrainProvider, range: ClosedRange<Double>,
                                       step: Terrain3DProgress.Step, tiles: inout Int,
                                       progress: @MainActor @Sendable (Terrain3DProgress) -> Void,
                                       query: (TerrainStore) -> TerrainAnswer<Value>,
                                       missing: (Value) -> Double?) async -> TerrainAnswer<Value>? {
        guard let store = terrain.store else { return nil }
        var answer = query(store)
        var rounds = 0
        while answer.status == .needDEM, !answer.needTiles.isEmpty, rounds < TerrainProvider.defaultMaxRounds,
              !Task.isCancelled {
            rounds += 1
            tiles += answer.needTiles.count
            let unknown: Double = answer.value.flatMap(missing) ?? 1
            let known = 1 - min(max(unknown, 0), 1)
            let fraction = range.lowerBound + (range.upperBound - range.lowerBound) * known * 0.9
            await progress(Terrain3DProgress(fraction: fraction, step: step, tiles: tiles))
            await terrain.load(answer.needTiles)
            answer = query(store)
        }
        return answer
    }

    /// The sheet's raster at the pyramid level nearest `longSide`, dark when the map is shown dark
    /// (the core's dark style over the whole sheet at once, as for the dark thumbnail).
    static func sheetImage(tileStore: TileStore, request: Request, longSide: Int) -> CGImage? {
        let level = TileStore.overviewLevel(width: request.sheetWidth, height: request.sheetHeight,
                                            levels: max(1, request.pyramidLevels), maxLongSide: longSide)
        guard let stitched = tileStore.stitchedLevel(level) else { return nil }
        if request.dark, let style = tileStore.darkStyle, let data = stitched.data {
            let pixels = UnsafeMutableRawBufferPointer(start: data, count: stitched.bytesPerRow * stitched.height)
            style.render(pixels, width: stitched.width, height: stitched.height, bytesPerRow: stitched.bytesPerRow,
                         u0: 0, v0: 0, du: 1, dv: 1)
        }
        return stitched.makeImage()
    }
}

// MARK: - Geo items

/// A place shown on the block: a POI of this map or another, a waypoint or a note.
struct Terrain3DMarker: Hashable, Sendable {
    enum Style: Hashable, Sendable {
        case poi(POIKind)
        case waypoint(WaypointSymbol)
        case note(photo: Bool)
    }

    var id: String
    var name: String
    var position: LatLon
    var style: Style
    /// Another map's place: drawn a little fainter.
    var isForeign: Bool = false
    /// Higher shows its label first.
    var priority: Int
}

/// A line draped on the block: a route (planned or GPX) or the recorded track.
struct Terrain3DLine: Sendable {
    var id: String
    var points: [LatLon]
    /// sRGB, 0…1.
    var color: SIMD4<Float>
    /// On screen, points.
    var width: Double

    static func rgba(_ color: UIColor) -> SIMD4<Float> {
        var r: CGFloat = 0
        var g: CGFloat = 0
        var b: CGFloat = 0
        var a: CGFloat = 0
        color.getRed(&r, green: &g, blue: &b, alpha: &a)
        return SIMD4(Float(r), Float(g), Float(b), Float(a))
    }
}

/// The geo items on the block (the track comes separately: it grows as you walk).
struct Terrain3DItems: Sendable {
    var markers: [Terrain3DMarker] = []
    var routes: [Terrain3DLine] = []

    /// Same name within this distance: one place (another map's label of a peak already shown).
    static let duplicateM = 150.0

    /// The label order of a POI kind (peaks, huts and places before spot heights).
    static func priority(_ kind: POIKind) -> Int {
        switch kind {
        case .peak, .waypoint: return 6
        case .place, .shelter: return 5
        case .viewpoint: return 4
        case .water, .parking: return 3
        case .spotHeight: return 2
        case .other: return 1
        }
    }

    /// Waypoints and notes (everything on the block), this map's POIs, then other located maps'
    /// POIs on the block that are not already shown; at most `maxMarkers`, the most important.
    @MainActor
    static func gather(map: MapDocument, area: Terrain3DArea, app: AppModel) -> Terrain3DItems {
        let settings = app.settings
        var markers: [Terrain3DMarker] = []
        func onBlock(_ p: LatLon) -> Bool { area.contains(gridPoint: area.gridPoint(for: p)) }

        for w in app.geoItems.waypoints(in: area.bounds) where onBlock(w.position) {
            markers.append(Terrain3DMarker(id: "waypoint-\(w.id.uuidString)", name: w.name, position: w.position,
                                           style: .waypoint(w.symbol), priority: 9))
        }
        for n in app.geoItems.notes(in: area.bounds) where onBlock(n.position) {
            markers.append(Terrain3DMarker(id: "note-\(n.id.uuidString)", name: n.title, position: n.position,
                                           style: .note(photo: n.photo != nil), priority: 8))
        }
        if settings.showPOIs {
            for poi in map.pois where onBlock(poi.position) {
                markers.append(Terrain3DMarker(id: "poi-\(poi.id.uuidString)", name: poi.name, position: poi.position,
                                               style: .poi(poi.kind), priority: priority(poi.kind)))
            }
            if settings.showOtherMapsPOIs {
                for other in app.library.maps where other.id != map.id && other.georef != nil {
                    for poi in other.pois where area.bounds.contains(poi.position) && onBlock(poi.position) {
                        guard !poi.name.isEmpty, !isDuplicate(poi.name, at: poi.position, in: markers) else { continue }
                        markers.append(Terrain3DMarker(id: "poi-\(other.id.uuidString)-\(poi.id.uuidString)",
                                                       name: poi.name, position: poi.position, style: .poi(poi.kind),
                                                       isForeign: true, priority: priority(poi.kind) - 1))
                    }
                }
            }
        }
        // Stable: equal priorities keep their order (waypoints, notes, this map, other maps).
        let ranked = markers.enumerated().sorted { a, b in
            a.element.priority != b.element.priority ? a.element.priority > b.element.priority : a.offset < b.offset
        }.map(\.element)

        let routes = app.geoItems.routes(touching: area.bounds).map { route in
            Terrain3DLine(id: "route-\(route.id.uuidString)", points: route.latLons,
                          color: Terrain3DLine.rgba(route.displayColor.uiColor), width: 4.5)
        }
        return Terrain3DItems(markers: Array(ranked.prefix(Terrain3DLimits.maxMarkers)), routes: routes)
    }

    static func isDuplicate(_ name: String, at p: LatLon, in markers: [Terrain3DMarker]) -> Bool {
        let key = name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return markers.contains { m in
            m.name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == key
                && Geodesy.distance(m.position, p) <= duplicateM
        }
    }
}

// MARK: - Session

/// The 3D screen's state: the build (progress, result, failure), the geo items on the block and
/// Retry. Use from the main thread.
@MainActor
@Observable
final class Terrain3DSession {
    enum Phase: Equatable {
        case building
        case ready
        case failed(String)
    }

    private(set) var phase: Phase = .building
    private(set) var progress = Terrain3DProgress.start
    private(set) var model: Terrain3DModel?
    private(set) var items = Terrain3DItems()
    /// Bumped whenever `items` changes (the scene re-drapes them).
    private(set) var itemsRevision = 0
    /// Bumped by Retry: the screen builds again.
    private(set) var attempt = 0
    /// Bumped by each finished build (a haptic).
    private(set) var builds = 0

    init() {}

    /// Builds the block for map `mapID` (cancelled with the calling task).
    func build(mapID: UUID, app: AppModel) async {
        guard let map = app.library.map(id: mapID), let georef = map.georef else {
            phase = .failed("Locate this map to see it in 3D")
            return
        }
        phase = .building
        progress = .start
        // The continuation margin (maps' rule); its own reduction of the same rule otherwise.
        let mapsMargin = Double(MapContinuation.marginPixels(for: map, enabled: true))
        let margin = mapsMargin > 0 && mapsMargin.isFinite ? mapsMargin
            : Terrain3DMargin.marginPixels(georef: georef, width: map.pixelWidth, height: map.pixelHeight)
        let request = Terrain3DBuilder.Request(
            mapID: map.id, georef: georef, sheetWidth: map.pixelWidth, sheetHeight: map.pixelHeight,
            pyramidLevels: map.levels, margin: margin, dark: app.settings.mapAppearance == .dark,
            maxZoom: min(13, app.battery.maxTerrainZoom),
            textureSide: Terrain3DLimits.textureSide(physicalMemory: ProcessInfo.processInfo.physicalMemory))
        let tileStore = TileStore(map: map, library: app.library)
        let terrain = app.terrain
        let report: @MainActor @Sendable (Terrain3DProgress) -> Void = { [weak self] p in
            self?.apply(p)
        }
        let work = Task.detached(priority: .userInitiated) {
            await Terrain3DBuilder.build(request, terrain: terrain, tileStore: tileStore, progress: report)
        }
        let built = await withTaskCancellationHandler {
            await work.value
        } onCancel: {
            work.cancel()
        }
        guard !Task.isCancelled else { return }
        guard let built else {
            phase = .failed("This map couldn't be shown in 3D")
            return
        }
        model = built
        refreshItems(mapID: mapID, app: app)
        phase = .ready
        builds += 1
    }

    /// The geo items on the block again (they changed, or a setting did).
    func refreshItems(mapID: UUID, app: AppModel) {
        guard let model, let map = app.library.map(id: mapID) else { return }
        items = Terrain3DItems.gather(map: map, area: model.area, app: app)
        itemsRevision += 1
    }

    /// Retry: forgets the tiles marked missing when the network is there (they are fetched again)
    /// and builds again.
    func retry(app: AppModel) {
        if NetworkPathObserver.isOnlineNow {
            app.terrain.store?.clearMissing()
        }
        attempt += 1
    }

    private func apply(_ p: Terrain3DProgress) {
        guard phase == .building else { return }
        if p.fraction >= progress.fraction || p.step != progress.step {
            progress = p
        }
    }
}
