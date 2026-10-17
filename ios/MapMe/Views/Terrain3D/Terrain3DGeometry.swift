import CoreGraphics
import Foundation

// The 3D view's numbers (docs/design/explorer.md, 3D view): which area the block covers, how
// finely it is meshed, where the map's pixels land on it, the heights under it, lines draped on
// it and the orbiting camera. Value types without UIKit or SceneKit, safe on any thread, so the
// builder runs them off the main thread and the unit tests check them directly.
//
// Local frame: metres on the ground around the sheet's centre, x east, z south, y up (heights
// above the block's lowest point; the scene scales y by the vertical exaggeration).

// MARK: - Limits

/// Sizes and limits that keep the 3D view within a phone's memory and frame budget.
enum Terrain3DLimits {
    /// Mesh nodes per side: one every two DEM cells of zoom 12, at least 96 (a small sheet still
    /// gets a smooth surface), at most 384 (147 k vertices, ~6 MB of vertex data).
    static let minNodes = 96
    static let maxNodes = 384
    /// DEM cells of the mesh's zoom between neighbouring nodes.
    static let cellsPerNode = 2.0
    /// The terrain zoom of the mesh (13 only where 96 nodes are finer than zoom 12's cells).
    static let meshZoom = 12
    /// The core's grid is sampled this much denser than the mesh (within the same zoom), so
    /// resampling it at the mesh's nodes loses little relief; at most `maxGridNodes` a side.
    static let gridOversampling = 1.5
    static let maxGridNodes = 576
    /// The coarse level of detail (shown while the block is small on screen) keeps every second
    /// node: a quarter of the triangles.
    static let coarseStep = 2
    /// Below this on-screen radius (points) the coarse mesh is drawn.
    static let coarseScreenRadius = 160.0
    /// Long side of the draped texture by device memory: 4096² RGBA is 64 MB (and the GPU's copy
    /// with mipmaps ~85 MB more), fine with 6 GB, too much with 3.
    static func textureSide(physicalMemory: UInt64) -> Int {
        let gb = Double(physicalMemory) / 1_073_741_824
        if gb >= 5.5 { return 4096 }
        if gb >= 3.5 { return 3072 }
        return 2048
    }
    static let minTextureSide = 64
    /// The sheet's own raster is stitched from the pyramid level at most this long (memory: a
    /// 4096 × 4096 stitch is another 64 MB while the texture is drawn).
    static let maxSheetRasterSide = 4096
    /// Tiles the texture's contours may draw on; past this the render is held to a coarser zoom
    /// (a 1:250 000 sheet would otherwise want hundreds of zoom-12 tiles).
    static let maxTextureTiles = 160
    /// The block's sides reach this far below the lowest terrain (a diorama).
    static let skirtDepthM = 50.0
    /// Lines float at least this far above the terrain (more when seen from far away).
    static let lineLiftM = 2.5
    /// Lift per metre of camera distance: ~30 m at 20 km, under a point on screen at 45°.
    static let lineLiftPerDistance = 0.0015
    /// Billboards (POIs, waypoints, notes) and their labels.
    static let maxMarkers = 200
    static let maxLabels = 14
    /// Labels nearer than this always show (when among the `maxLabels` most important).
    static let labelNearM = 600.0
    /// Draped line samples per line (a 100 k point GPX track is simplified first).
    static let maxLineSamples = 60_000
    /// Vertical exaggeration: 1.5× by default, 1…3× from the menu.
    static let defaultExaggeration = 1.5
    static let exaggerationChoices: [Double] = [1, 1.5, 2, 3]
    static let exaggerationRange: ClosedRange<Double> = 1...3
    /// More than this share of the terrain unknown: the block is shown flat (filling would invent
    /// most of the relief) with "Terrain for this area isn't on your iPhone yet".
    static let flatAboveMissing = 0.5
    /// Up to this share unknown counts as complete (a few ocean or edge nodes).
    static let completeBelowMissing = 0.02
}

// MARK: - Continuation margin

/// How far the block reaches beyond the sheet: the continuation margin (docs/design/explorer.md
/// §5, `terrain::continuation_margin_m`). `MapContinuation.marginPixels` (maps) is used when it
/// gives one; this is the same rule for when it does not (0: the feature not filled in, or the
/// canvas setting off; the 3D view always shows the surroundings).
enum Terrain3DMargin {
    static let minM = 3_000.0
    static let maxM = 15_000.0
    /// A homography's projective denominator at the enlarged corners stays within ±25 % of its
    /// value at the centre (a photo's projection diverges away from the sheet).
    static let maxPerspectiveChange = 0.25
    /// Each failed check shrinks the margin by this factor (at most `maxShrinks` times, then 0).
    static let shrink = 0.7
    static let maxShrinks = 12

    /// Ground metres: half the sheet's long side, 3…15 km.
    static func marginMeters(longSideM: Double) -> Double {
        guard longSideM.isFinite else { return minM }
        return min(max(longSideM / 2, minM), maxM)
    }

    /// The margin in map pixels (converted at the sheet's centre), reduced until the projection
    /// stays sane over the enlarged rectangle. 0 for an unusable georeference.
    static func marginPixels(georef: Georeference, width: Int, height: Int) -> Double {
        let w = Double(width)
        let h = Double(height)
        guard w > 0, h > 0 else { return 0 }
        let mpp = georef.metersPerPixel(atPixel: CGPoint(x: w / 2, y: h / 2))
        guard mpp.isFinite, mpp > 0 else { return 0 }
        var margin = marginMeters(longSideM: max(w, h) * mpp) / mpp
        let m = georef.pixelToMercator.m
        let w0 = m[6] * (w / 2) + m[7] * (h / 2) + m[8]
        guard w0.isFinite, w0 != 0 else { return 0 }
        for _ in 0..<maxShrinks {
            if perspectiveIsSane(m, w0: w0, x0: -margin, y0: -margin, x1: w + margin, y1: h + margin) {
                return margin
            }
            margin *= shrink
        }
        return 0
    }

    /// Projective denominators at the rectangle's corners within ±25 % of `w0`.
    static func perspectiveIsSane(_ m: [Double], w0: Double, x0: Double, y0: Double, x1: Double, y1: Double) -> Bool {
        for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
            let r = (m[6] * x + m[7] * y + m[8]) / w0
            guard r.isFinite, abs(r - 1) <= maxPerspectiveChange else { return false }
        }
        return true
    }
}

// MARK: - Grid rules

/// How finely the block is meshed and which terrain zoom it asks for.
enum Terrain3DGridRule {
    /// Nodes along a side spanning `spanMercator` Mercator metres: one every two cells of zoom 12
    /// (plus the first), within 96…384.
    static func nodeCount(spanMercator: Double) -> Int {
        let cell = TileMath.metersPerPixel(zoom: Terrain3DLimits.meshZoom)
        guard spanMercator.isFinite, spanMercator > 0 else { return Terrain3DLimits.minNodes }
        let wanted = spanMercator / (Terrain3DLimits.cellsPerNode * cell)
        let n = wanted >= Double(Terrain3DLimits.maxNodes) ? Terrain3DLimits.maxNodes : Int(wanted.rounded(.down)) + 1
        return min(max(n, Terrain3DLimits.minNodes), Terrain3DLimits.maxNodes)
    }

    /// The mesh's terrain zoom: 12, or 13 where its nodes are closer than two zoom-12 cells (a
    /// small sheet held at 96 nodes); never above `maxZoom` (the battery saver's 12).
    static func meshZoom(nodeSpacingMercator: Double, maxZoom: Int) -> Int {
        let cell12 = TileMath.metersPerPixel(zoom: Terrain3DLimits.meshZoom)
        let finer = nodeSpacingMercator.isFinite && nodeSpacingMercator < Terrain3DLimits.cellsPerNode * cell12 * 0.999
        let zoom = finer ? Terrain3DLimits.meshZoom + 1 : Terrain3DLimits.meshZoom
        return min(zoom, max(maxZoom, 8))
    }

    /// The core grid's nodes over `rect`: the mesh's spacing ÷ `gridOversampling`, 2…576 a side.
    static func coreGridSize(rect: MercatorRect, nodeSpacingMercator: Double) -> (cols: Int, rows: Int) {
        let spacing = nodeSpacingMercator / Terrain3DLimits.gridOversampling
        func count(_ span: Double) -> Int {
            guard span.isFinite, span > 0, spacing.isFinite, spacing > 0 else { return 2 }
            let wanted = span / spacing
            if wanted >= Double(Terrain3DLimits.maxGridNodes) { return Terrain3DLimits.maxGridNodes }
            return min(max(Int(wanted.rounded(.up)) + 1, 2), Terrain3DLimits.maxGridNodes)
        }
        return (count(rect.width), count(rect.height))
    }
}

// MARK: - Area

/// Where the 3D block lies: the sheet grown by the continuation margin, in the map's own pixels,
/// and the local frame the scene is built in.
///
/// The mesh is laid out on the map's pixel grid: node (i, j) is a pixel of the enlarged sheet,
/// placed on Earth by the georeference (homography and warp, `Georeference.mercator(forPixel:)`).
/// Its texture coordinate is that pixel ÷ the enlarged size, so the map is draped exactly, warp
/// included, and continues into the terrain texture beyond its edge. Geo items find their place
/// on the block through the inverse (`gridPoint(for:)`: homography inverse and warp iteration).
/// A Mercator-aligned mesh would put corners outside the enlarged sheet for a rotated map or a
/// photo, where neither the texture nor a photo's projection has anything to say.
struct Terrain3DArea: Sendable {
    let georef: Georeference
    /// The sheet, base-image pixels.
    let sheetWidth: Double
    let sheetHeight: Double
    /// Map pixels added on every side.
    let margin: Double
    /// Mesh nodes along the map's x (columns) and y (rows).
    let cols: Int
    let rows: Int
    /// Mercator position of the local frame's origin: the sheet's centre.
    let origin: SIMD2<Double>
    /// Ground metres per Mercator metre at the origin (cos φ₀).
    let groundScale: Double
    /// The enlarged sheet's Mercator bounding box (the core grid's rectangle).
    let mercatorRect: MercatorRect
    /// Its latitude/longitude box (which geo items to gather).
    let bounds: GeoBounds
    /// Mercator metres between neighbouring nodes (the larger of the two directions).
    let nodeSpacingMercator: Double
    /// The sheet's size on the ground through its centre (metres, x then y).
    let sheetGroundSize: SIMD2<Double>
    private let inverse: Homography

    var enlargedWidth: Double { sheetWidth + 2 * margin }
    var enlargedHeight: Double { sheetHeight + 2 * margin }

    /// Nil when the georeference does not place the enlarged sheet (non-finite projection).
    init?(georef: Georeference, sheetWidth: Int, sheetHeight: Int, margin: Double) {
        guard sheetWidth > 0, sheetHeight > 0, margin.isFinite, margin >= 0 else { return nil }
        let w = Double(sheetWidth)
        let h = Double(sheetHeight)
        // The enlarged rectangle's outline on Earth: its edges sampled, since a warp bends them.
        var outline: [SIMD2<Double>] = []
        let steps = 16
        for k in 0...steps {
            let t = Double(k) / Double(steps)
            let x = -margin + t * (w + 2 * margin)
            let y = -margin + t * (h + 2 * margin)
            outline.append(georef.mercator(forPixel: SIMD2(x, -margin)))
            outline.append(georef.mercator(forPixel: SIMD2(x, h + margin)))
            outline.append(georef.mercator(forPixel: SIMD2(-margin, y)))
            outline.append(georef.mercator(forPixel: SIMD2(w + margin, y)))
        }
        guard outline.allSatisfy({ $0.x.isFinite && $0.y.isFinite }),
              let rect = MercatorRect(enclosing: outline), rect.width > 0, rect.height > 0,
              let box = GeoBounds.enclosing(outline.map { WebMercator.unproject($0) })
        else { return nil }
        let centre = georef.mercator(forPixel: SIMD2(w / 2, h / 2))
        guard centre.x.isFinite, centre.y.isFinite else { return nil }
        let scale = WebMercator.groundPerMercator(lat: WebMercator.unproject(centre).lat)
        guard scale.isFinite, scale > 0 else { return nil }

        // Nodes along each pixel axis from the enlarged sheet's span through its centre.
        let spanX = Self.length(georef.mercator(forPixel: SIMD2(w + margin, h / 2))
            - georef.mercator(forPixel: SIMD2(-margin, h / 2)))
        let spanY = Self.length(georef.mercator(forPixel: SIMD2(w / 2, h + margin))
            - georef.mercator(forPixel: SIMD2(w / 2, -margin)))
        guard spanX.isFinite, spanY.isFinite, spanX > 0, spanY > 0 else { return nil }
        let cols = Terrain3DGridRule.nodeCount(spanMercator: spanX)
        let rows = Terrain3DGridRule.nodeCount(spanMercator: spanY)

        let sheetX = Self.length(georef.mercator(forPixel: SIMD2(w, h / 2)) - georef.mercator(forPixel: SIMD2(0, h / 2)))
        let sheetY = Self.length(georef.mercator(forPixel: SIMD2(w / 2, h)) - georef.mercator(forPixel: SIMD2(w / 2, 0)))

        self.georef = georef
        self.sheetWidth = w
        self.sheetHeight = h
        self.margin = margin
        self.cols = cols
        self.rows = rows
        origin = centre
        groundScale = scale
        mercatorRect = rect
        bounds = box
        nodeSpacingMercator = max(spanX / Double(cols - 1), spanY / Double(rows - 1))
        sheetGroundSize = SIMD2(sheetX * scale, sheetY * scale)
        inverse = georef.mercatorToPixel
    }

    private static func length(_ v: SIMD2<Double>) -> Double { (v.x * v.x + v.y * v.y).squareRoot() }

    // Map pixels, grid coordinates, texture coordinates.

    /// Map pixel of grid point (column, row), fractional.
    func pixel(col: Double, row: Double) -> SIMD2<Double> {
        SIMD2(-margin + col * enlargedWidth / Double(cols - 1), -margin + row * enlargedHeight / Double(rows - 1))
    }

    /// Grid coordinates (fractional column, row) of a map pixel.
    func gridPoint(pixel p: SIMD2<Double>) -> SIMD2<Double> {
        SIMD2((p.x + margin) / enlargedWidth * Double(cols - 1), (p.y + margin) / enlargedHeight * Double(rows - 1))
    }

    /// Texture coordinate of a map pixel: (0, 0) the enlarged sheet's top-left corner, (1, 1) its
    /// bottom-right (SceneKit's texture origin is the image's top-left).
    func uv(pixel p: SIMD2<Double>) -> SIMD2<Double> {
        SIMD2((p.x + margin) / enlargedWidth, (p.y + margin) / enlargedHeight)
    }

    /// Map pixel of a coordinate: the homography's inverse, then the warp's fixed-point iteration.
    func pixel(for p: LatLon) -> SIMD2<Double> {
        georef.pixel(forMercator: WebMercator.project(p), inverse: inverse)
    }

    func uv(for p: LatLon) -> SIMD2<Double> { uv(pixel: pixel(for: p)) }

    func gridPoint(for p: LatLon) -> SIMD2<Double> { gridPoint(pixel: pixel(for: p)) }

    /// On the block (the enlarged sheet), grid coordinates.
    func contains(gridPoint g: SIMD2<Double>) -> Bool {
        g.x.isFinite && g.y.isFinite && g.x >= 0 && g.y >= 0 && g.x <= Double(cols - 1) && g.y <= Double(rows - 1)
    }

    /// Inside the sheet itself (not its margin).
    func sheetContains(_ p: LatLon) -> Bool {
        let px = pixel(for: p)
        return px.x.isFinite && px.y.isFinite && px.x >= 0 && px.y >= 0 && px.x <= sheetWidth && px.y <= sheetHeight
    }

    // The local frame.

    /// Local ground position (x east, z south, metres) of a Mercator point.
    func local(mercator m: SIMD2<Double>) -> SIMD2<Double> {
        SIMD2((m.x - origin.x) * groundScale, (origin.y - m.y) * groundScale)
    }

    func mercator(local l: SIMD2<Double>) -> SIMD2<Double> {
        SIMD2(origin.x + l.x / groundScale, origin.y - l.y / groundScale)
    }

    func local(for p: LatLon) -> SIMD2<Double> { local(mercator: WebMercator.project(p)) }

    /// Local ground positions of every node, row by row (forward projection with the warp).
    func localNodes() -> [SIMD2<Double>] {
        var out: [SIMD2<Double>] = []
        out.reserveCapacity(cols * rows)
        for j in 0..<rows {
            for i in 0..<cols {
                out.append(local(mercator: georef.mercator(forPixel: pixel(col: Double(i), row: Double(j)))))
            }
        }
        return out
    }

    /// The sheet's rectangle in a texture of `width` × `height` pixels (top-left origin).
    func sheetRect(textureWidth width: Int, height: Int) -> CGRect {
        let sx = Double(width) / enlargedWidth
        let sy = Double(height) / enlargedHeight
        return CGRect(x: margin * sx, y: margin * sy, width: sheetWidth * sx, height: sheetHeight * sy)
    }

    /// Ground metres per map pixel at the sheet's centre.
    var metersPerPixel: Double {
        sheetWidth > 0 ? sheetGroundSize.x / sheetWidth : 0
    }

    /// The same block on the same ground (the same sheet, margin, nodes and local frame): a camera
    /// over one stands over the same place on the other (another appearance, Retry).
    func hasSameFrame(as other: Terrain3DArea) -> Bool {
        sheetWidth == other.sheetWidth && sheetHeight == other.sheetHeight && margin == other.margin
            && cols == other.cols && rows == other.rows && origin == other.origin && groundScale == other.groundScale
    }
}

// MARK: - Texture

/// The draped texture's size and the zoom its contours are drawn from.
enum Terrain3DTextureRule {
    /// Pixels of a texture of the enlarged sheet: its long side `maxSide` (never more texels than
    /// map pixels: no point upscaling a small image), the other side in proportion, at least 64.
    static func size(enlargedWidth: Double, enlargedHeight: Double, maxSide: Int) -> (width: Int, height: Int) {
        let longSide = max(enlargedWidth, enlargedHeight)
        guard longSide.isFinite, longSide > 0 else { return (Terrain3DLimits.minTextureSide, Terrain3DLimits.minTextureSide) }
        let side = min(Double(max(maxSide, Terrain3DLimits.minTextureSide)), longSide.rounded(.up))
        let scale = side / longSide
        let w = max(Terrain3DLimits.minTextureSide, Int((enlargedWidth * scale).rounded()))
        let h = max(Terrain3DLimits.minTextureSide, Int((enlargedHeight * scale).rounded()))
        return (w, h)
    }

    /// The zoom the core's render uses for texels of `texelGroundM` ground metres (its rule: the
    /// coarsest zoom whose cell is at most two output pixels), within `maxZoom`.
    static func renderZoom(texelGroundM: Double, groundScale: Double, maxZoom: Int) -> Int {
        guard texelGroundM.isFinite, texelGroundM > 0, groundScale > 0 else { return maxZoom }
        var z = 0
        while z < maxZoom, TileMath.metersPerPixel(zoom: z) * groundScale > 2 * texelGroundM {
            z += 1
        }
        return z
    }

    /// The pyramid level to drape: the finest whose long side is at most 1.5× the sheet's size in
    /// the texture (and `maxSheetRasterSide`), so it is drawn at most a little reduced.
    static func sheetLongSide(sheetTexels: Double) -> Int {
        let wanted = Int((sheetTexels * 1.5).rounded(.up))
        return min(max(wanted, 512), Terrain3DLimits.maxSheetRasterSide)
    }

    /// Contour line widths: buffer pixels per point (minor lines 1.4 texels wide, index lines 2.2:
    /// they stay visible when the texture is reduced and lit, and crisp when it is enlarged).
    static let lineScale = 2.0

    /// The contour interval for texels of `texelGroundM` ground metres: the core's automatic one
    /// at a texel and a half per point, as before the lines widened (at `lineScale` it would
    /// double on some maps).
    static func contourInterval(texelGroundM: Double) -> Double {
        ContourInterval.automatic(metersPerPoint: texelGroundM * 1.5)
    }
}

// MARK: - Heights

/// Heights under the mesh from the core's grid.
enum Terrain3DHeights {
    /// Bilinear height of a core grid at a Mercator point; unknown (NaN) nodes are left out of
    /// the weights. NaN outside the grid or where all four nodes around the point are unknown.
    static func sample(_ grid: HeightGrid, at m: SIMD2<Double>) -> Float {
        guard grid.cols >= 2, grid.rows >= 2, grid.heights.count >= grid.cols * grid.rows,
              grid.rect.width > 0, grid.rect.height > 0, m.x.isFinite, m.y.isFinite
        else { return .nan }
        let fx = (m.x - grid.rect.minX) / grid.rect.width * Double(grid.cols - 1)
        let fy = (grid.rect.maxY - m.y) / grid.rect.height * Double(grid.rows - 1)
        let eps = 1e-9
        guard fx >= -eps, fy >= -eps, fx <= Double(grid.cols - 1) + eps, fy <= Double(grid.rows - 1) + eps else { return .nan }
        let i0 = min(max(Int(fx.rounded(.down)), 0), grid.cols - 2)
        let j0 = min(max(Int(fy.rounded(.down)), 0), grid.rows - 2)
        let u = min(max(fx - Double(i0), 0), 1)
        let v = min(max(fy - Double(j0), 0), 1)
        let corners: [(Int, Int, Double)] = [
            (i0, j0, (1 - u) * (1 - v)), (i0 + 1, j0, u * (1 - v)),
            (i0, j0 + 1, (1 - u) * v), (i0 + 1, j0 + 1, u * v),
        ]
        var sum = 0.0
        var weight = 0.0
        for (i, j, wt) in corners {
            let value = grid.heights[j * grid.cols + i]
            guard value.isFinite else { continue }
            sum += Double(value) * wt
            weight += wt
        }
        // No weight on a known node: the point sits on unknown ones.
        guard weight > 1e-9 else { return .nan }
        return Float(sum / weight)
    }

    /// Heights (metres) at the mesh's nodes, row by row; NaN where unknown (no grid: all NaN).
    static func meshHeights(area: Terrain3DArea, grid: HeightGrid?) -> [Float] {
        var out = [Float](repeating: .nan, count: area.cols * area.rows)
        guard let grid else { return out }
        for j in 0..<area.rows {
            for i in 0..<area.cols {
                let m = area.georef.mercator(forPixel: area.pixel(col: Double(i), row: Double(j)))
                out[j * area.cols + i] = sample(grid, at: m)
            }
        }
        return out
    }

    /// Fills unknown heights from their known neighbours, breadth first from the known ones, so
    /// a hole takes the values around it. All unknown: 0 everywhere (a flat block). Returns the
    /// share of nodes that were unknown.
    @discardableResult
    static func fillGaps(_ heights: inout [Float], cols: Int, rows: Int) -> Double {
        let n = cols * rows
        guard n > 0, heights.count == n else { return 1 }
        var known = [Bool](repeating: false, count: n)
        var queue: [Int] = []
        queue.reserveCapacity(n)
        for k in 0..<n where heights[k].isFinite {
            known[k] = true
            queue.append(k)
        }
        let unknown = n - queue.count
        guard unknown > 0 else { return 0 }
        guard !queue.isEmpty else {
            for k in 0..<n { heights[k] = 0 }
            return 1
        }
        var head = 0
        while head < queue.count {
            let k = queue[head]
            head += 1
            let i = k % cols
            let j = k / cols
            for (ni, nj) in [(i - 1, j), (i + 1, j), (i, j - 1), (i, j + 1)] {
                guard ni >= 0, nj >= 0, ni < cols, nj < rows else { continue }
                let nk = nj * cols + ni
                guard !known[nk] else { continue }
                // The mean of its neighbours known so far (at least the one it was reached from).
                var sum: Float = 0
                var count: Float = 0
                for (ai, aj) in [(ni - 1, nj), (ni + 1, nj), (ni, nj - 1), (ni, nj + 1)] {
                    guard ai >= 0, aj >= 0, ai < cols, aj < rows else { continue }
                    let ak = aj * cols + ai
                    if known[ak] {
                        sum += heights[ak]
                        count += 1
                    }
                }
                heights[nk] = count > 0 ? sum / count : heights[k]
                known[nk] = true
                queue.append(nk)
            }
        }
        return Double(unknown) / Double(n)
    }
}

// MARK: - Mesh

/// The block's geometry: the draped surface (positions, normals, texture coordinates, triangles)
/// and its four sides down below the lowest terrain. Flat arrays, ready for SceneKit's sources.
struct Terrain3DMesh: Sendable {
    /// Nodes along each side.
    let cols: Int
    let rows: Int
    /// x, y, z per vertex: local metres, y = height above `baseM` (not exaggerated).
    var positions: [Float]
    var normals: [Float]
    /// u, v per vertex.
    var uvs: [Float]
    var indices: [UInt32]
    /// The sides: x, y, z per vertex, top and bottom vertices alternating around the block.
    var skirtPositions: [Float]
    /// 0 at the top edge, 1 at the bottom, per skirt vertex (its colour ramp).
    var skirtShade: [Float]
    var skirtIndices: [UInt32]

    var vertexCount: Int { positions.count / 3 }
    var triangleCount: Int { indices.count / 3 }

    /// The node indices kept along a side of `count` nodes with `step` (always the last one).
    static func kept(_ count: Int, step: Int) -> [Int] {
        guard count > 0 else { return [] }
        var out = Array(stride(from: 0, to: count, by: max(1, step)))
        if out.last != count - 1 { out.append(count - 1) }
        return out
    }

    /// The mesh over every `step`-th node of a `cols` × `rows` lattice. `nodes`: local ground
    /// positions (x, z) per node; `heights`: metres per node, gaps filled; `baseM`: the height
    /// the block's y counts from (its lowest terrain).
    static func build(cols: Int, rows: Int, nodes: [SIMD2<Double>], heights: [Float], baseM: Float,
                      step: Int = 1) -> Terrain3DMesh {
        let iKeep = kept(cols, step: step)
        let jKeep = kept(rows, step: step)
        let mc = iKeep.count
        let mr = jKeep.count
        let count = mc * mr
        var positions = [Float](repeating: 0, count: count * 3)
        var uvs = [Float](repeating: 0, count: count * 2)
        for (b, j) in jKeep.enumerated() {
            for (a, i) in iKeep.enumerated() {
                let k = j * cols + i
                let v = b * mc + a
                let node = nodes[k]
                let h = heights[k].isFinite ? heights[k] - baseM : 0
                positions[3 * v] = Float(node.x)
                positions[3 * v + 1] = h
                positions[3 * v + 2] = Float(node.y)
                uvs[2 * v] = Float(Double(i) / Double(max(cols - 1, 1)))
                uvs[2 * v + 1] = Float(Double(j) / Double(max(rows - 1, 1)))
            }
        }
        // Two triangles per cell along the (i, j)–(i+1, j+1) diagonal, counter-clockwise seen
        // from above for a map whose x runs east and y south (the material is double-sided
        // anyway, and normals are turned up below).
        var indices: [UInt32] = []
        indices.reserveCapacity(max(0, (mc - 1) * (mr - 1) * 6))
        if mc >= 2, mr >= 2 {
            for b in 0..<(mr - 1) {
                for a in 0..<(mc - 1) {
                    let v00 = UInt32(b * mc + a)
                    let v10 = v00 + 1
                    let v01 = UInt32((b + 1) * mc + a)
                    let v11 = v01 + 1
                    indices.append(contentsOf: [v00, v11, v10, v00, v01, v11])
                }
            }
        }
        let normals = vertexNormals(positions: positions, indices: indices)

        // The sides: around the edge (top row west→east, east column, bottom row east→west,
        // west column), each edge vertex with one straight below it.
        var ring: [Int] = []
        for a in 0..<mc { ring.append(a) }
        for b in 1..<max(mr, 1) { ring.append(b * mc + (mc - 1)) }
        if mr >= 2 { for a in stride(from: mc - 2, through: 0, by: -1) { ring.append((mr - 1) * mc + a) } }
        if mc >= 2 { for b in stride(from: mr - 2, through: 1, by: -1) { ring.append(b * mc) } }
        var skirtPositions: [Float] = []
        var skirtShade: [Float] = []
        var skirtIndices: [UInt32] = []
        let bottom = Float(-Terrain3DLimits.skirtDepthM)
        if ring.count >= 3 {
            skirtPositions.reserveCapacity(ring.count * 6)
            for v in ring {
                skirtPositions.append(contentsOf: [positions[3 * v], positions[3 * v + 1], positions[3 * v + 2]])
                skirtPositions.append(contentsOf: [positions[3 * v], bottom, positions[3 * v + 2]])
                skirtShade.append(contentsOf: [0, 1])
            }
            let n = UInt32(ring.count)
            for r in 0..<n {
                let next = (r + 1) % n
                let top0 = 2 * r
                let bottom0 = 2 * r + 1
                let top1 = 2 * next
                let bottom1 = 2 * next + 1
                skirtIndices.append(contentsOf: [top0, bottom0, top1, top1, bottom0, bottom1])
            }
        }
        return Terrain3DMesh(cols: mc, rows: mr, positions: positions, normals: normals, uvs: uvs, indices: indices,
                             skirtPositions: skirtPositions, skirtShade: skirtShade, skirtIndices: skirtIndices)
    }

    /// Smooth normals: each triangle's (area-weighted) normal added to its corners, turned up
    /// (terrain never faces down), then normalised.
    static func vertexNormals(positions: [Float], indices: [UInt32]) -> [Float] {
        var sums = [SIMD3<Double>](repeating: SIMD3(0, 0, 0), count: positions.count / 3)
        var t = 0
        while t + 2 < indices.count {
            let a = Int(indices[t])
            let b = Int(indices[t + 1])
            let c = Int(indices[t + 2])
            t += 3
            let pa = SIMD3(Double(positions[3 * a]), Double(positions[3 * a + 1]), Double(positions[3 * a + 2]))
            let pb = SIMD3(Double(positions[3 * b]), Double(positions[3 * b + 1]), Double(positions[3 * b + 2]))
            let pc = SIMD3(Double(positions[3 * c]), Double(positions[3 * c + 1]), Double(positions[3 * c + 2]))
            var n = Self.cross(pb - pa, pc - pa)
            if n.y < 0 { n = -n }
            sums[a] += n
            sums[b] += n
            sums[c] += n
        }
        var out = [Float](repeating: 0, count: positions.count)
        for (k, s) in sums.enumerated() {
            let len = (s.x * s.x + s.y * s.y + s.z * s.z).squareRoot()
            let n = len > 1e-12 ? s / len : SIMD3<Double>(0, 1, 0)
            out[3 * k] = Float(n.x)
            out[3 * k + 1] = Float(n.y)
            out[3 * k + 2] = Float(n.z)
        }
        return out
    }

    private static func cross(_ a: SIMD3<Double>, _ b: SIMD3<Double>) -> SIMD3<Double> {
        SIMD3(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
    }
}

// MARK: - Surface

/// One point of a line draped on the block: its grid coordinates and its place on the surface
/// (x, height above the base, z).
struct Terrain3DDrapedPoint: Equatable, Sendable {
    var grid: SIMD2<Double>
    var local: SIMD3<Double>
}

/// The block's surface for placing things on it: the full-resolution mesh's nodes and the same
/// triangles, so a point found here lies exactly on what is drawn.
struct Terrain3DSurface: Sendable {
    let cols: Int
    let rows: Int
    /// Local ground positions (x, z) per node, row by row.
    let nodes: [SIMD2<Double>]
    /// Heights above the base per node (metres, not exaggerated).
    let heights: [Float]
    /// An affine fit of grid coordinates from local ground positions (the camera's clearance and
    /// target height: anywhere on the block, a few metres off at most).
    private let toGrid: (origin: SIMD2<Double>, col: SIMD2<Double>, row: SIMD2<Double>)

    init(cols: Int, rows: Int, nodes: [SIMD2<Double>], heights: [Float]) {
        self.cols = cols
        self.rows = rows
        self.nodes = nodes
        self.heights = heights
        // grid = A⁻¹ (local − p00), A's columns the local steps of one column and one row.
        let p00 = nodes.first ?? SIMD2(0, 0)
        let pc = cols > 1 && nodes.count >= cols ? nodes[cols - 1] : p00 + SIMD2(1, 0)
        let pr = rows > 1 && nodes.count >= cols * rows ? nodes[(rows - 1) * cols] : p00 + SIMD2(0, 1)
        let a = (pc - p00) / Double(max(cols - 1, 1))
        let b = (pr - p00) / Double(max(rows - 1, 1))
        let det = a.x * b.y - a.y * b.x
        if det.isFinite, abs(det) > 1e-12 {
            // Rows of A⁻¹.
            toGrid = (p00, SIMD2(b.y / det, -b.x / det), SIMD2(-a.y / det, a.x / det))
        } else {
            toGrid = (p00, SIMD2(0, 0), SIMD2(0, 0))
        }
    }

    /// The point of the surface at grid coordinates (exact on the mesh's triangles); nil off the
    /// block.
    func point(at g: SIMD2<Double>) -> SIMD3<Double>? {
        guard cols >= 2, rows >= 2, g.x.isFinite, g.y.isFinite else { return nil }
        let eps = 1e-9
        guard g.x >= -eps, g.y >= -eps, g.x <= Double(cols - 1) + eps, g.y <= Double(rows - 1) + eps else { return nil }
        let i = min(max(Int(g.x.rounded(.down)), 0), cols - 2)
        let j = min(max(Int(g.y.rounded(.down)), 0), rows - 2)
        let fu = min(max(g.x - Double(i), 0), 1)
        let fv = min(max(g.y - Double(j), 0), 1)
        let p00 = vertex(i, j)
        let p10 = vertex(i + 1, j)
        let p01 = vertex(i, j + 1)
        let p11 = vertex(i + 1, j + 1)
        // The cell's two triangles meet along its (i, j)–(i+1, j+1) diagonal.
        if fu >= fv {
            return p00 + (p10 - p00) * fu + (p11 - p10) * fv
        }
        return p00 + (p01 - p00) * fv + (p11 - p01) * fu
    }

    /// Height above the base at grid coordinates; nil off the block.
    func height(at g: SIMD2<Double>) -> Double? { point(at: g)?.y }

    private func vertex(_ i: Int, _ j: Int) -> SIMD3<Double> {
        let k = j * cols + i
        let n = nodes[k]
        return SIMD3(n.x, Double(heights[k]), n.y)
    }

    /// Approximate grid coordinates of a local ground position (the affine fit).
    func gridPoint(local l: SIMD2<Double>) -> SIMD2<Double> {
        let d = l - toGrid.origin
        return SIMD2(toGrid.col.x * d.x + toGrid.col.y * d.y, toGrid.row.x * d.x + toGrid.row.y * d.y)
    }

    /// The grid offset of a local ground offset `d` near grid point `g` (the cell's own linear
    /// map, exact within its triangles' plane of the ground).
    func gridOffset(local d: SIMD2<Double>, near g: SIMD2<Double>) -> SIMD2<Double> {
        guard cols >= 2, rows >= 2, g.x.isFinite, g.y.isFinite else { return SIMD2(0, 0) }
        let i = min(max(Int(g.x.rounded(.down)), 0), cols - 2)
        let j = min(max(Int(g.y.rounded(.down)), 0), rows - 2)
        let p00 = nodes[j * cols + i]
        let a = nodes[j * cols + i + 1] - p00
        let b = nodes[(j + 1) * cols + i] - p00
        let det = a.x * b.y - a.y * b.x
        guard det.isFinite, abs(det) > 1e-12 else { return SIMD2(0, 0) }
        return SIMD2((b.y * d.x - b.x * d.y) / det, (-a.y * d.x + a.x * d.y) / det)
    }

    /// The line through `points` (grid coordinates) laid on the surface: split wherever it crosses
    /// a triangle edge (the grid's columns, rows and the cells' diagonals), so every piece between
    /// two samples lies in one triangle and the polyline lies on the surface. Parts off the block
    /// are dropped (a line leaving and coming back makes two pieces); non-finite points break it.
    func drape(_ points: [SIMD2<Double>], maxSamples: Int = Terrain3DLimits.maxLineSamples) -> [[Terrain3DDrapedPoint]] {
        var pieces: [[Terrain3DDrapedPoint]] = []
        var current: [Terrain3DDrapedPoint] = []
        var total = 0
        func flush() {
            if current.count >= 2 { pieces.append(current) }
            current = []
        }
        func add(_ g: SIMD2<Double>) {
            guard let p = point(at: g) else { return }
            if let last = current.last {
                let d = last.grid - g
                if d.x * d.x + d.y * d.y < 1e-14 { return }
            }
            current.append(Terrain3DDrapedPoint(grid: g, local: p))
            total += 1
        }
        guard points.count >= 2 else { return [] }
        for k in 0..<(points.count - 1) {
            let a = points[k]
            let b = points[k + 1]
            guard a.x.isFinite, a.y.isFinite, b.x.isFinite, b.y.isFinite, let (t0, t1) = clip(a, b) else {
                flush()
                continue
            }
            let s = a + (b - a) * t0
            let e = a + (b - a) * t1
            if t0 > 0 { flush() }
            if current.isEmpty { add(s) }
            for t in crossings(s, e) { add(s + (e - s) * t) }
            add(e)
            if t1 < 1 { flush() }
            if total >= maxSamples { break }
        }
        flush()
        return pieces
    }

    /// Where a segment crosses the triangles' edges: parameters in (0, 1), in order.
    func crossings(_ s: SIMD2<Double>, _ e: SIMD2<Double>) -> [Double] {
        var ts: [Double] = []
        func lines(_ a: Double, _ b: Double) {
            guard a != b, a.isFinite, b.isFinite else { return }
            let lo = min(a, b)
            let hi = max(a, b)
            var k = lo.rounded(.down) + 1
            while k < hi {
                ts.append((k - a) / (b - a))
                k += 1
            }
        }
        lines(s.x, e.x)
        lines(s.y, e.y)
        lines(s.x - s.y, e.x - e.y)
        ts.sort()
        return ts
    }

    /// The part of segment a→b on the block (Liang–Barsky): its parameter interval, nil if none.
    func clip(_ a: SIMD2<Double>, _ b: SIMD2<Double>) -> (Double, Double)? {
        var t0 = 0.0
        var t1 = 1.0
        let d = b - a
        let maxX = Double(cols - 1)
        let maxY = Double(rows - 1)
        // Inside where p·t ≤ q for all four edges.
        for (p, q) in [(-d.x, a.x), (d.x, maxX - a.x), (-d.y, a.y), (d.y, maxY - a.y)] {
            if p == 0 {
                if q < 0 { return nil }
                continue
            }
            let r = q / p
            if p < 0 {
                if r > t1 { return nil }
                if r > t0 { t0 = r }
            } else {
                if r < t0 { return nil }
                if r < t1 { t1 = r }
            }
        }
        return t0 <= t1 ? (t0, t1) : nil
    }
}

// MARK: - Lines

/// Lines on the block: simplification before draping, ribbons after.
enum Terrain3DLines {
    /// Douglas–Peucker (iterative): drops points within `tolerance` of the line through their
    /// neighbours, keeping the ends. Non-finite points are kept (they break the line later).
    static func simplify(_ points: [SIMD2<Double>], tolerance: Double) -> [SIMD2<Double>] {
        guard points.count > 2, tolerance > 0 else { return points }
        var keep = [Bool](repeating: false, count: points.count)
        keep[0] = true
        keep[points.count - 1] = true
        var stack: [(Int, Int)] = [(0, points.count - 1)]
        let tol2 = tolerance * tolerance
        while let (first, last) = stack.popLast() {
            guard last > first + 1 else { continue }
            let a = points[first]
            let b = points[last]
            let ab = b - a
            let len2 = ab.x * ab.x + ab.y * ab.y
            var worst = -1.0
            var worstIndex = -1
            for k in (first + 1)..<last {
                let p = points[k]
                guard p.x.isFinite, p.y.isFinite else {
                    worst = .infinity
                    worstIndex = k
                    break
                }
                let ap = p - a
                let bp = p - b
                var d2: Double = ap.x * ap.x + ap.y * ap.y
                if len2 > 0 {
                    let t: Double = (ap.x * ab.x + ap.y * ab.y) / len2
                    if t > 1 {
                        d2 = bp.x * bp.x + bp.y * bp.y
                    } else if t >= 0 {
                        let c: Double = ab.x * ap.y - ab.y * ap.x
                        d2 = c * c / len2
                    }
                }
                if d2 > worst {
                    worst = d2
                    worstIndex = k
                }
            }
            if worst > tol2, worstIndex > first {
                keep[worstIndex] = true
                stack.append((first, worstIndex))
                stack.append((worstIndex, last))
            }
        }
        var out: [SIMD2<Double>] = []
        out.reserveCapacity(points.count)
        for (k, p) in points.enumerated() where keep[k] { out.append(p) }
        return out
    }

    /// A flat ribbon along a draped piece: `width` metres wide, each side sampled on the surface
    /// itself (so it follows side slopes), lifted by `lift`, heights scaled by `exaggeration`.
    /// Returns x, y, z per vertex (left and right alternating) and the triangles.
    static func ribbon(_ piece: [Terrain3DDrapedPoint], surface: Terrain3DSurface, width: Double, lift: Double,
                       exaggeration: Double) -> (positions: [Float], indices: [UInt32]) {
        let n = piece.count
        guard n >= 2, width > 0 else { return ([], []) }
        var positions: [Float] = []
        positions.reserveCapacity(n * 6)
        var indices: [UInt32] = []
        indices.reserveCapacity((n - 1) * 6)
        let half = width / 2
        for k in 0..<n {
            let here = piece[k]
            let prev = piece[max(k - 1, 0)].local
            let next = piece[min(k + 1, n - 1)].local
            var tx = next.x - prev.x
            var tz = next.z - prev.z
            let len = (tx * tx + tz * tz).squareRoot()
            if len > 1e-9 {
                tx /= len
                tz /= len
            } else {
                tx = 1
                tz = 0
            }
            // Left of the direction of travel, seen from above (x east, z south).
            let side = SIMD2<Double>(tz, -tx) * half
            for offset in [side, -side] {
                let g = here.grid + surface.gridOffset(local: offset, near: here.grid)
                let h = surface.height(at: g) ?? here.local.y
                positions.append(Float(here.local.x + offset.x))
                positions.append(Float(h * exaggeration + lift))
                positions.append(Float(here.local.z + offset.y))
            }
        }
        for k in 0..<(n - 1) {
            let l0 = UInt32(2 * k)
            let r0 = l0 + 1
            let l1 = l0 + 2
            let r1 = l0 + 3
            indices.append(contentsOf: [l0, r0, l1, r0, r1, l1])
        }
        return (positions, indices)
    }

    /// The lift of lines above the terrain seen from `distance` metres: enough that the terrain
    /// between samples never pokes through, under a point on screen.
    static func lift(cameraDistance distance: Double) -> Double {
        max(Terrain3DLimits.lineLiftM, distance * Terrain3DLimits.lineLiftPerDistance)
    }
}

// MARK: - Camera

/// The orbiting camera: it looks at `target` from `distance` metres, along compass heading `yaw`
/// and `pitch` below the horizon (Apple Maps' 3D camera, turntable style: no roll).
struct Terrain3DOrbit: Equatable, Sendable {
    /// What the camera looks at (scene coordinates: y exaggerated).
    var target: SIMD3<Double>
    /// Direction the camera looks, radians clockwise from north.
    var yaw: Double
    /// Angle below the horizon, radians.
    var pitch: Double
    var distance: Double

    static let minPitch = 12.0 * Double.pi / 180
    static let maxPitch = 89.0 * Double.pi / 180
    /// It starts looking north at 45° (docs/design/explorer.md, 3D view: Camera).
    static let defaultPitch = 45.0 * Double.pi / 180
    static let minDistance = 250.0

    /// Unit vector the camera looks along.
    var forward: SIMD3<Double> {
        SIMD3(sin(yaw) * cos(pitch), -sin(pitch), -cos(yaw) * cos(pitch))
    }

    /// Where the camera is.
    var position: SIMD3<Double> { target - forward * distance }

    /// The camera's right, on the ground (x, z).
    var groundRight: SIMD2<Double> { SIMD2(cos(yaw), sin(yaw)) }

    /// The camera's forward, on the ground (x, z).
    var groundForward: SIMD2<Double> { SIMD2(sin(yaw), -cos(yaw)) }

    /// Compass heading of the view, degrees 0..<360.
    var headingDegrees: Double { Self.normalizedDegrees(yaw * 180 / .pi) }

    static func normalizedDegrees(_ d: Double) -> Double {
        guard d.isFinite else { return 0 }
        let r = d.truncatingRemainder(dividingBy: 360)
        return r < 0 ? r + 360 : r
    }

    /// The camera `t` (0…1) of the way to `other`: the yaw turns the short way, the distance
    /// changes geometrically (a zoom feels even).
    func interpolated(to other: Terrain3DOrbit, _ t: Double) -> Terrain3DOrbit {
        let s = min(max(t, 0), 1)
        let dYaw = MapAngle.normalized(other.yaw - yaw)
        let d0 = max(distance, 1e-3)
        let d1 = max(other.distance, 1e-3)
        return Terrain3DOrbit(target: target + (other.target - target) * s,
                              yaw: yaw + dYaw * s,
                              pitch: pitch + (other.pitch - pitch) * s,
                              distance: d0 * pow(d1 / d0, s))
    }

    /// Pitch and distance within their limits (`maxDistance` from the block's size).
    func clamped(maxDistance: Double) -> Terrain3DOrbit {
        var o = self
        o.pitch = min(max(pitch, Self.minPitch), Self.maxPitch)
        o.distance = min(max(distance, Self.minDistance), max(maxDistance, Self.minDistance))
        if !o.yaw.isFinite { o.yaw = 0 }
        return o
    }

    /// Where scene point `p` lands in a view `size` points large whose field of view spans its
    /// shorter side; nil when it is behind the camera.
    func screenPoint(of p: SIMD3<Double>, fovDegrees: Double, size: CGSize) -> CGPoint? {
        let f = forward
        // The camera's right and up, as SceneKit's look-at with world up +y builds them.
        let side = SIMD3<Double>(-f.z, 0, f.x)
        let sideLength = (side.x * side.x + side.z * side.z).squareRoot()
        guard sideLength > 1e-9, size.width > 0, size.height > 0 else { return nil }
        let right = side / sideLength
        let up = SIMD3<Double>(right.y * f.z - right.z * f.y, right.z * f.x - right.x * f.z, right.x * f.y - right.y * f.x)
        let v = p - position
        let depth = (v * f).sum()
        guard depth > 1e-6 else { return nil }
        let w = Double(size.width)
        let h = Double(size.height)
        let focal = min(w, h) / 2 / tan(fovDegrees * .pi / 360)
        let x = w / 2 + (v * right).sum() / depth * focal
        let y = h / 2 - (v * up).sum() / depth * focal
        return CGPoint(x: x, y: y)
    }

    /// Ground metres per point at `distance` for a field of view of `fovDegrees` across
    /// `viewPoints` points: what keeps billboards and line widths a constant size on screen.
    static func metersPerPoint(distance: Double, fovDegrees: Double, viewPoints: Double) -> Double {
        guard viewPoints > 0, distance.isFinite else { return 1 }
        return 2 * distance * tan(fovDegrees * .pi / 360) / viewPoints
    }

    /// The distance that shows a block `extent` metres across in a `fovDegrees` view, with room.
    static func fitDistance(extent: Double, fovDegrees: Double) -> Double {
        let half = max(extent, 100) / 2
        return half / tan(fovDegrees * .pi / 360) * 1.15
    }
}

// MARK: - Decluttering

/// Which billboards to draw, as the 2D map places its POIs: greedily, the most important first
/// (then the ones already shown, so a turning view doesn't flicker, then the nearest), skipping a
/// disc that would cover one already placed and a label that would cover anything placed.
enum Terrain3DDeclutter {
    struct Candidate: Equatable {
        /// The disc's centre on screen, points; nil when behind the camera.
        var disc: CGPoint?
        /// The label's size, points; nil without a label or out of label range.
        var label: CGSize?
        var priority: Int
        var distance: Double
        var wasShown: Bool = false
        /// Screen size over nominal: billboards are sized by distance, the view projects by depth,
        /// so ones off the view's centre line come out larger.
        var scale: CGFloat = 1
    }

    struct Placement: Equatable {
        var disc: Bool
        var label: Bool
    }

    /// Gap kept between a disc and its label above it (points).
    static let labelGap: CGFloat = 2

    /// `reserved`: screen rectangles nothing may cover (you).
    static func place(_ candidates: [Candidate], markerSize: CGFloat, viewSize: CGSize,
                      maxLabels: Int, reserved: [CGRect] = []) -> [Placement] {
        var result = [Placement](repeating: Placement(disc: false, label: false), count: candidates.count)
        let order: [Int] = candidates.indices.sorted { i, j in
            let a = candidates[i]
            let b = candidates[j]
            if a.priority != b.priority { return a.priority > b.priority }
            if a.wasShown != b.wasShown { return a.wasShown }
            if a.distance != b.distance { return a.distance < b.distance }
            return i < j
        }
        let screen = CGRect(origin: .zero, size: viewSize).insetBy(dx: -markerSize, dy: -markerSize)
        var taken: [CGRect] = reserved
        taken.reserveCapacity(reserved.count + candidates.count * 2)
        var labels = 0
        for i in order {
            let c = candidates[i]
            guard let centre = c.disc, centre.x.isFinite, centre.y.isFinite else { continue }
            // Off screen: drawn (nothing shows) but takes no room.
            guard screen.contains(centre) else {
                result[i].disc = true
                continue
            }
            let m = markerSize * c.scale
            let disc = CGRect(x: centre.x - m / 2, y: centre.y - m / 2, width: m, height: m).insetBy(dx: -2, dy: -2)
            if taken.contains(where: { $0.intersects(disc) }) { continue }
            taken.append(disc)
            result[i].disc = true
            guard let label = c.label, labels < maxLabels else { continue }
            let size = CGSize(width: label.width * c.scale, height: label.height * c.scale)
            let box = CGRect(x: centre.x - size.width / 2, y: centre.y - m / 2 - labelGap * c.scale - size.height,
                             width: size.width, height: size.height).insetBy(dx: 2, dy: 2)
            if taken.contains(where: { $0.intersects(box) }) { continue }
            taken.append(box)
            result[i].label = true
            labels += 1
        }
        return result
    }
}
