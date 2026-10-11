import QuartzCore
import UIKit

/// The map raster as a CATiledLayer-backed view (Apple PhotoScroller pattern). Its bounds are the
/// base image size (1 map pixel = 1 point); the canvas places it with `center` + `transform`.
/// Tiles are drawn on CATiledLayer's background threads from the pre-built pyramid in TileStore.
final class TiledMapView: UIView {
    override class var layerClass: AnyClass { CATiledLayer.self }

    /// Immutable snapshot read by the drawing threads.
    private struct RenderState {
        let store: TileStore
        let tileSize: Int
        let levels: Int
        let dark: Bool
    }

    private let lock = NSLock()
    private var renderState: RenderState?
    private var mapID: UUID?

    /// Tile edge in backing-store pixels.
    static let backingTileSize: CGFloat = 512
    /// Deepest zoom the layer renders sharply: 2^bias × 1:1.
    static let detailBias = 3

    private var tiledLayer: CATiledLayer {
        // layerClass guarantees the type.
        layer as! CATiledLayer
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
        isUserInteractionEnabled = false
        tiledLayer.tileSize = CGSize(width: Self.backingTileSize, height: Self.backingTileSize)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Point the view at a map and its tiles. Cheap when nothing changed.
    func configure(map: MapDocument, store: TileStore, dark: Bool) {
        let levels = max(1, map.levels)
        let tileSize = max(1, map.tileSize)
        let sameMap = mapID == map.id && renderState.map { $0.store === store } == true
        if sameMap, let current = renderState, current.dark == dark { return }

        lock.lock()
        renderState = RenderState(store: store, tileSize: tileSize, levels: levels, dark: dark)
        lock.unlock()

        if !sameMap {
            mapID = map.id
            bounds = CGRect(origin: .zero, size: map.pixelSize)
            // Coarser levels than the pyramid's last are still useful when zoomed far out.
            tiledLayer.levelsOfDetail = levels + 2 + Self.detailBias
            tiledLayer.levelsOfDetailBias = Self.detailBias
        }
        tiledLayer.setNeedsDisplay()
    }

    override func draw(_ rect: CGRect) {
        guard let ctx = UIGraphicsGetCurrentContext() else { return }
        lock.lock()
        let snapshot = renderState
        lock.unlock()
        guard let state = snapshot else { return }

        let ctm = ctx.ctm
        let scale = sqrt(ctm.a * ctm.a + ctm.b * ctm.b)
        let level = Self.pyramidLevel(forScale: scale, levels: state.levels)
        let factor = CGFloat(1 << level)
        let span = CGFloat(state.tileSize) * factor
        let levelSize = state.store.levelSize(level)
        let cols = Int(ceil(levelSize.width / CGFloat(state.tileSize)))
        let rows = Int(ceil(levelSize.height / CGFloat(state.tileSize)))
        guard cols > 0, rows > 0 else { return }

        let firstCol = max(0, Int(floor(rect.minX / span)))
        let lastCol = min(cols - 1, Int(floor((rect.maxX - 0.001) / span)))
        let firstRow = max(0, Int(floor(rect.minY / span)))
        let lastRow = min(rows - 1, Int(floor((rect.maxY - 0.001) / span)))
        guard firstCol <= lastCol, firstRow <= lastRow else { return }

        ctx.interpolationQuality = .medium
        for row in firstRow...lastRow {
            for col in firstCol...lastCol {
                guard let tile = state.store.tile(level: level, col: col, row: row, dark: state.dark) else { continue }
                let tileRect = CGRect(x: CGFloat(col) * span, y: CGFloat(row) * span,
                                      width: CGFloat(tile.width) * factor, height: CGFloat(tile.height) * factor)
                UIImage(cgImage: tile).draw(in: tileRect)
            }
        }
    }

    /// Pyramid level whose pixels best match `scale` device pixels per map pixel
    /// (never coarser than the screen, so the map stays sharp).
    static func pyramidLevel(forScale scale: CGFloat, levels: Int) -> Int {
        guard scale > 0, scale.isFinite else { return 0 }
        let l = Int(floor(log2(1 / scale) + 1e-3))
        return min(max(l, 0), max(0, levels - 1))
    }
}
