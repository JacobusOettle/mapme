import Observation
import QuartzCore
import UIKit

/// What the terrain beyond the sheet shows now: the walked-off banner says it honestly ("showing
/// terrain", "loading", "not on your iPhone").
@MainActor
@Observable
final class OffMapTerrainStatus {
    enum State: Equatable {
        /// No continuation (map not located, the setting off).
        case off
        /// Waiting for terrain tiles.
        case loading
        /// Contours are drawn.
        case shown
        /// No terrain could be had for what is on screen (offline and not downloaded).
        case unavailable
    }

    private(set) var state: State = .off

    func set(_ value: State) {
        if value != state { state = value }
    }
}

/// Terrain contours beyond the sheet's edge, in the map's pixel space (a CATiledLayer under the
/// sheet, tiles drawn by the core's `mm_terrain_render`).
///
/// docs/design/explorer.md §7.3: the view covers the sheet grown by the continuation margin on
/// every side (`bounds` with a negative origin, so the canvas centres it on the sheet's centre);
/// tiles are rendered in the map's own projection (homography and warp), so the terrain's contours
/// meet the sheet's at the edge; the background is a shade off the sheet's paper, dark or paper
/// as the map. Tiles lacking terrain ask `TerrainProvider.prefetch` and are drawn again when it
/// arrives. Tiles wholly under the sheet are skipped (the sheet covers them).
@MainActor
final class OffMapTerrainLayer: CanvasMapSpaceLayer {
    let view = UIView()
    let isBelowSheet = true
    /// Read by the walked-off banner.
    let status = OffMapTerrainStatus()
    private let tiles = OffMapTerrainTiledView(frame: .zero)
    private weak var model: AppModel?
    private weak var context: MapScreenContext?
    private var mapContext: CanvasMapContext?
    private var batteryLoop: ObservationLoop?
    /// Terrain arriving later (a region or a map's trailhead finished, the network is back).
    private var arrival: TerrainArrivalObserver?
    private var terrainZoomCap = 13

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        view.isHidden = true
        view.isUserInteractionEnabled = false
        view.backgroundColor = .clear
        tiles.isUserInteractionEnabled = false
        view.addSubview(tiles)
        tiles.onStatus = { [weak self] state in self?.status.set(state) }
        // The battery saver caps the terrain's detail; redraw when it changes.
        batteryLoop = ObservationLoop { [weak self] in
            self?.batteryChanged()
        }
        // Tiles drawn without terrain (or out of fetch rounds) draw again once it is there.
        arrival = TerrainArrivalObserver(model.terrain) { [weak self] in
            self?.tiles.reload()
        }
    }

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        configure()
    }

    private func batteryChanged() {
        guard let model else { return }
        let cap = model.battery.maxTerrainZoom
        guard cap != terrainZoomCap else { return }
        terrainZoomCap = cap
        configure()
    }

    private func configure() {
        guard let c = mapContext, let georef = c.georef, let provider = model?.terrain, c.continuationMargin > 0,
              c.map.pixelWidth > 0, c.map.pixelHeight > 0
        else {
            tiles.configure(nil)
            view.isHidden = true
            status.set(.off)
            return
        }
        let m = c.continuationMargin
        let sheet = c.sheetRect
        let outer = sheet.insetBy(dx: -m, dy: -m)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        view.bounds = outer
        // The tiled view keeps a zero-origin bounds (CATiledLayer's own grid); its frame in the
        // container's coordinates is the enlarged rectangle, so map pixel = its point − margin.
        tiles.frame = outer
        CATransaction.commit()

        let mid = CGPoint(x: sheet.midX, y: sheet.midY)
        let mpp = georef.metersPerPixel(atPixel: mid)
        let latitude = georef.latLon(forPixel: mid).lat
        let screenScale = view.traitCollection.displayScale > 0 ? view.traitCollection.displayScale : 3
        let dark = c.darkMap
        let config = OffMapTerrainTiledView.Config(
            mapID: c.map.id,
            margin: m,
            sheetSize: sheet.size,
            homography: georef.pixelToMercator.m,
            warp: georef.warp,
            style: dark ? .dark : .paper,
            background: TerrainContourRenderer.rgba(dark ? Theme.uiOffMapNightPaper : Theme.uiOffMapPaper),
            backgroundColor: (dark ? Theme.uiOffMapNightPaper : Theme.uiOffMapPaper).cgColor,
            hillshade: dark ? 0.25 : 0.35,
            screenScale: screenScale,
            metersPerPixel: mpp.isFinite && mpp > 0 ? mpp : 1,
            latitude: latitude.isFinite ? latitude : 0,
            terrainZoomCap: terrainZoomCap,
            provider: provider)
        if tiles.configure(config) { status.set(.loading) }
        view.isHidden = false
    }
}

/// The CATiledLayer drawing the terrain (on its own threads; see `OffMapTerrainLayer`).
final class OffMapTerrainTiledView: UIView {
    override class var layerClass: AnyClass { FadingTiledLayer.self }

    /// Everything a drawing thread needs, set on the main thread and read under the lock.
    struct Config {
        var mapID: UUID
        /// Map pixels beyond each edge of the sheet.
        var margin: CGFloat
        var sheetSize: CGSize
        var homography: [Double]
        var warp: MeshWarp?
        var style: ContourRenderRequest.Style
        var background: [Int]
        var backgroundColor: CGColor
        var hillshade: Double
        var screenScale: CGFloat
        /// Ground metres per map pixel at the sheet's centre (one scale per level, so the tiles of
        /// a level agree on the contour interval and line widths).
        var metersPerPixel: Double
        var latitude: Double
        var terrainZoomCap: Int
        var provider: TerrainProvider
    }

    /// Called on the main thread when what the tiles show changes.
    var onStatus: (@MainActor (OffMapTerrainStatus.State) -> Void)?

    /// Rounds of fetching terrain for one tile before it stays as drawn (like
    /// `TerrainProvider.defaultMaxRounds`): tiles that cannot be had are marked missing, so the
    /// engine falls back to coarser zooms and answers with what there is.
    static let maxFetchRounds = 6
    static let tileSide: CGFloat = 512
    /// Sharp up to 2³ × 1:1 (the canvas zooms to 6×).
    static let detailBias = 3

    private let lock = NSLock()
    private var config: Config?
    private var generation = 0
    /// Fetch rounds per tile (key: generation, level and position).
    private var rounds: [String: Int] = [:]
    /// Tiles drawn with terrain / waiting for terrain / drawn without, this generation.
    private var counts = (shown: 0, waiting: Set<String>(), empty: 0)
    private var reported: OffMapTerrainStatus.State?

    private var tiledLayer: CATiledLayer {
        // layerClass guarantees the type.
        layer as! CATiledLayer
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
        tiledLayer.tileSize = CGSize(width: Self.tileSide, height: Self.tileSide)
        tiledLayer.levelsOfDetailBias = Self.detailBias
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// New map, appearance, margin or detail cap (nil: draw nothing). Redraws everything; false
    /// when nothing changed (nothing is redrawn then).
    @discardableResult
    func configure(_ new: Config?) -> Bool {
        lock.lock()
        let same: Bool
        switch (config, new) {
        case (nil, nil):
            same = true
        case let (o?, n?):
            same = o.mapID == n.mapID && o.margin == n.margin && o.sheetSize == n.sheetSize
                && o.homography == n.homography && o.warp == n.warp && o.style == n.style
                && o.terrainZoomCap == n.terrainZoomCap && o.screenScale == n.screenScale
        default:
            same = false
        }
        if !same {
            config = new
            generation += 1
            rounds = [:]
            counts = (shown: 0, waiting: [], empty: 0)
            reported = nil
        }
        lock.unlock()
        guard !same else { return false }
        if let new {
            // Zoomed out to the whole enlarged sheet on a phone: about 2^k smaller than 1:1.
            let side = max(new.sheetSize.width, new.sheetSize.height) + 2 * new.margin
            let zoomOut = min(max(Int(ceil(log2(max(1, side / 256)))), 1), 14)
            tiledLayer.levelsOfDetail = Self.detailBias + zoomOut + 1
        }
        layer.contents = nil
        setNeedsDisplay()
        return true
    }

    /// Draws every tile again with fresh fetch rounds: terrain the tiles lacked may be on the
    /// device now (`TerrainProvider.terrainDidArrive`). Nothing when not configured.
    func reload() {
        lock.lock()
        guard config != nil else {
            lock.unlock()
            return
        }
        generation += 1
        rounds = [:]
        counts = (shown: 0, waiting: [], empty: 0)
        reported = nil
        lock.unlock()
        setNeedsDisplay()
    }

    override func draw(_ rect: CGRect) {
        guard let ctx = UIGraphicsGetCurrentContext() else { return }
        lock.lock()
        let snapshot = config
        let gen = generation
        lock.unlock()
        guard let cfg = snapshot else { return }

        ctx.setFillColor(cfg.backgroundColor)
        ctx.fill(rect)
        let m = cfg.margin
        let sheet = CGRect(x: m, y: m, width: cfg.sheetSize.width, height: cfg.sheetSize.height)
        // Under the sheet: its paper and tiles cover this.
        if sheet.contains(rect) { return }
        guard let store = cfg.provider.store else {
            note(.empty, key: "", generation: gen)
            return
        }

        // Device pixels per point of this view at the level being drawn.
        let ctm = ctx.ctm
        let scale = sqrt(ctm.a * ctm.a + ctm.b * ctm.b)
        guard scale.isFinite, scale > 0 else { return }
        let width = Int((rect.width * scale).rounded())
        let height = Int((rect.height * scale).rounded())
        guard width > 0, height > 0, width <= 4096, height <= 4096 else { return }
        // On screen, a map pixel is about `scale / screenScale` points at this level.
        let metersPerPoint = cfg.metersPerPixel * Double(cfg.screenScale / scale)
        let request = ContourRenderRequest(
            space: .map,
            rect: [Double(rect.minX - m), Double(rect.minY - m), Double(rect.maxX - m), Double(rect.maxY - m)],
            pixelToMercator: cfg.homography,
            warp: cfg.warp,
            style: cfg.style,
            intervalM: nil,
            scaleMPerPt: metersPerPoint,
            indexEvery: 5,
            hillshade: cfg.hillshade,
            lineScale: Double(cfg.screenScale),
            background: cfg.background,
            maxZoom: TerrainContourRenderer.demZoom(metersPerPoint: metersPerPoint, latitude: cfg.latitude,
                                                    cap: cfg.terrainZoomCap),
            minZoom: nil,
            maxTiles: nil)
        let result = TerrainContourRenderer.render(request, width: width, height: height, store: store)
        if let image = result.image {
            UIImage(cgImage: image).draw(in: rect)
        }

        let key = "\(gen)|\(Int(rect.minX.rounded()))|\(Int(rect.minY.rounded()))|\(Int(rect.width.rounded()))"
        let wanted = result.wantsTiles
        if !wanted.isEmpty, takeRound(key, generation: gen) {
            note(.waiting, key: key, generation: gen)
            cfg.provider.prefetch(wanted) { [weak self] in
                Task { @MainActor [weak self] in
                    self?.redraw(rect, generation: gen)
                }
            }
        } else {
            note(result.hasTerrain ? .shown : .empty, key: key, generation: gen)
        }
    }

    private enum TileOutcome {
        case shown, waiting, empty
    }

    /// True when the tile may fetch terrain once more.
    private func takeRound(_ key: String, generation gen: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard gen == generation else { return false }
        let n = rounds[key, default: 0]
        guard n < Self.maxFetchRounds else { return false }
        rounds[key] = n + 1
        return true
    }

    /// Counts what a tile shows and reports the overall state when it changes.
    private func note(_ outcome: TileOutcome, key: String, generation gen: Int) {
        lock.lock()
        guard gen == generation else {
            lock.unlock()
            return
        }
        switch outcome {
        case .waiting:
            counts.waiting.insert(key)
        case .shown:
            counts.waiting.remove(key)
            counts.shown += 1
        case .empty:
            counts.waiting.remove(key)
            counts.empty += 1
        }
        let state: OffMapTerrainStatus.State
        if counts.shown > 0 {
            state = .shown
        } else if !counts.waiting.isEmpty {
            state = .loading
        } else {
            state = .unavailable
        }
        let changed = state != reported
        if changed { reported = state }
        lock.unlock()
        guard changed else { return }
        Task { @MainActor [weak self] in
            self?.report(state, generation: gen)
        }
    }

    private func report(_ state: OffMapTerrainStatus.State, generation gen: Int) {
        lock.lock()
        let current = gen == generation
        lock.unlock()
        if current { onStatus?(state) }
    }

    private func redraw(_ rect: CGRect, generation gen: Int) {
        lock.lock()
        let current = gen == generation
        lock.unlock()
        if current { setNeedsDisplay(rect) }
    }
}
