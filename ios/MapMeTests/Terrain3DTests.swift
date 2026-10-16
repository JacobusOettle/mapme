import CoreGraphics
import XCTest
@testable import MapMe

/// The 3D view's numbers (docs/design/explorer.md, 3D view): the block's area and mesh size, the
/// map's texture coordinates through the georeference and its warp, heights under the mesh, lines
/// draped on it, ribbons, the texture's size and the orbiting camera. Synthetic georeferences and
/// height grids, no terrain engine: these are the parts the build is made of.
final class Terrain3DTests: XCTestCase {
    /// A sheet like the Grootkloof sample: 3507 × 4960 px at 1.9057 m/px around (−25.85, 27.40).
    static let centre = LatLon(lat: -25.85, lon: 27.40)
    static let sheetWidth = 3507
    static let sheetHeight = 4960
    static let groundMpp = 1.9057

    /// pixel → Mercator: `rotation` radians turned, `g` perspective along x (w = 1 at the centre,
    /// as a photo of a sheet would be), an optional warp.
    static func georef(mpp: Double = groundMpp, width: Int = sheetWidth, height: Int = sheetHeight,
                       rotation: Double = 0, g: Double = 0, warp: MeshWarp? = nil) -> Georeference {
        let c = WebMercator.project(centre)
        let s = mpp / WebMercator.groundPerMercator(lat: centre.lat)
        let hw = Double(width) / 2
        let hh = Double(height) / 2
        let co = cos(rotation)
        let si = sin(rotation)
        // Local offsets from the centre, then translated: M = T(c) · P.
        let p = [s * co, s * si, -s * co * hw - s * si * hh,
                 s * si, -s * co, -s * si * hw + s * co * hh,
                 g, 0, 1 - g * hw]
        let m = [p[0] + c.x * p[6], p[1] + c.x * p[7], p[2] + c.x * p[8],
                 p[3] + c.y * p[6], p[4] + c.y * p[7], p[5] + c.y * p[8],
                 p[6], p[7], p[8]]
        return Georeference(pixelToMercator: Homography(m), method: .grid, confidence: 1, rmsMeters: 2, warp: warp)
    }

    /// A smooth warp of up to ~30 Mercator metres over the sheet (paper that does not lie flat).
    static func warp(width: Int = sheetWidth, height: Int = sheetHeight) -> MeshWarp {
        var offsets: [Double] = []
        for j in 0..<5 {
            for i in 0..<4 {
                offsets.append(30 * sin(Double(i) * 1.3 + Double(j) * 0.4))
                offsets.append(-22 * cos(Double(i) * 0.7 - Double(j) * 0.9))
            }
        }
        return MeshWarp(cols: 4, rows: 5, width: Double(width), height: Double(height), offsets: offsets)
    }

    /// Heights linear in Mercator metres over a rectangle (bilinear and triangle interpolation are
    /// exact on it).
    static func planeGrid(_ rect: MercatorRect, cols: Int = 40, rows: Int = 50) -> HeightGrid {
        var heights: [Float] = []
        for j in 0..<rows {
            for i in 0..<cols {
                let x = rect.minX + Double(i) * rect.width / Double(cols - 1)
                let y = rect.maxY - Double(j) * rect.height / Double(rows - 1)
                heights.append(Float(plane(SIMD2(x, y), rect)))
            }
        }
        return HeightGrid(rect: rect, cols: cols, rows: rows, heights: heights, zoom: 12, minM: nil, maxM: nil,
                          missingFraction: 0)
    }

    static func plane(_ m: SIMD2<Double>, _ rect: MercatorRect) -> Double {
        1000 + 0.004 * (m.x - rect.minX) + 0.003 * (m.y - rect.minY)
    }

    /// A small lattice: 5 × 4 nodes 100 m apart, x east, z south.
    static func lattice(heights: (Int, Int) -> Float) -> (nodes: [SIMD2<Double>], heights: [Float]) {
        var nodes: [SIMD2<Double>] = []
        var values: [Float] = []
        for j in 0..<4 {
            for i in 0..<5 {
                nodes.append(SIMD2(Double(i) * 100, Double(j) * 100))
                values.append(heights(i, j))
            }
        }
        return (nodes, values)
    }

    // MARK: - Area

    func testMarginIsHalfTheLongSideWithinThreeToFifteenKilometres() {
        let sample = Self.georef()
        // 4960 px is the long side: half of it, in pixels, whatever the scale.
        XCTAssertEqual(Terrain3DMargin.marginPixels(georef: sample, width: Self.sheetWidth, height: Self.sheetHeight),
                       2480, accuracy: 1e-6)

        let tiny = Self.georef(width: 100, height: 100)
        let tinyMpp = tiny.metersPerPixel(atPixel: CGPoint(x: 50, y: 50))
        XCTAssertEqual(Terrain3DMargin.marginPixels(georef: tiny, width: 100, height: 100), 3000 / tinyMpp, accuracy: 1e-6)

        let huge = Self.georef(mpp: 25, width: 20000, height: 16000)
        let hugeMpp = huge.metersPerPixel(atPixel: CGPoint(x: 10000, y: 8000))
        XCTAssertEqual(Terrain3DMargin.marginPixels(georef: huge, width: 20000, height: 16000), 15000 / hugeMpp,
                       accuracy: 1e-6)

        XCTAssertEqual(Terrain3DMargin.marginMeters(longSideM: 2_000), 3_000)
        XCTAssertEqual(Terrain3DMargin.marginMeters(longSideM: 10_000), 5_000)
        XCTAssertEqual(Terrain3DMargin.marginMeters(longSideM: 150_000), 15_000)
        XCTAssertEqual(Terrain3DMargin.marginMeters(longSideM: .infinity), 3_000)
    }

    /// A photo's homography: the margin shrinks (×0.7 steps) until the projective denominator at
    /// the enlarged corners stays within ±25 % of the centre's.
    func testMarginShrinksWherePerspectiveWouldDiverge() {
        let photo = Self.georef(g: 1e-4)
        let margin = Terrain3DMargin.marginPixels(georef: photo, width: Self.sheetWidth, height: Self.sheetHeight)
        // 1e-4 × (1753.5 + m) ≤ 0.25 → m ≤ 746: 2480 · 0.7⁴.
        XCTAssertEqual(margin, 2480 * pow(0.7, 4), accuracy: 0.5)
        let w = Double(Self.sheetWidth)
        let h = Double(Self.sheetHeight)
        let m = photo.pixelToMercator.m
        XCTAssertTrue(Terrain3DMargin.perspectiveIsSane(m, w0: 1, x0: -margin, y0: -margin, x1: w + margin, y1: h + margin))
        XCTAssertFalse(Terrain3DMargin.perspectiveIsSane(m, w0: 1, x0: -margin / 0.7, y0: -margin / 0.7,
                                                         x1: w + margin / 0.7, y1: h + margin / 0.7))
    }

    func testMeshSizeAndTerrainZoomFollowTheBlock() throws {
        // The sample: 8467 × 9920 px enlarged, 17.9 × 21.0 km in Mercator metres: a node every two
        // zoom-12 cells (76.4 m).
        let sample = try XCTUnwrap(Terrain3DArea(georef: Self.georef(), sheetWidth: Self.sheetWidth,
                                                 sheetHeight: Self.sheetHeight, margin: 2480))
        XCTAssertEqual(sample.cols, 235)
        XCTAssertEqual(sample.rows, 275)
        XCTAssertEqual(Terrain3DGridRule.meshZoom(nodeSpacingMercator: sample.nodeSpacingMercator, maxZoom: 13), 12)
        XCTAssertEqual(sample.enlargedWidth, 3507 + 2 * 2480)
        XCTAssertEqual(sample.sheetGroundSize.y, 4960 * Self.groundMpp, accuracy: 1)

        // A small sheet keeps 96 nodes, finer than zoom 12's cells: zoom 13, unless the battery
        // saver holds terrain at 12.
        let tinyGeoref = Self.georef(width: 100, height: 100)
        let tinyMargin = Terrain3DMargin.marginPixels(georef: tinyGeoref, width: 100, height: 100)
        let tiny = try XCTUnwrap(Terrain3DArea(georef: tinyGeoref, sheetWidth: 100, sheetHeight: 100, margin: tinyMargin))
        XCTAssertEqual(tiny.cols, Terrain3DLimits.minNodes)
        XCTAssertEqual(tiny.rows, Terrain3DLimits.minNodes)
        XCTAssertEqual(Terrain3DGridRule.meshZoom(nodeSpacingMercator: tiny.nodeSpacingMercator, maxZoom: 13), 13)
        XCTAssertEqual(Terrain3DGridRule.meshZoom(nodeSpacingMercator: tiny.nodeSpacingMercator, maxZoom: 12), 12)

        // A 1:250 000 sheet: at most 384 a side.
        let huge = try XCTUnwrap(Terrain3DArea(georef: Self.georef(mpp: 25, width: 20000, height: 16000),
                                               sheetWidth: 20000, sheetHeight: 16000, margin: 600))
        XCTAssertEqual(huge.cols, Terrain3DLimits.maxNodes)
        XCTAssertEqual(huge.rows, Terrain3DLimits.maxNodes)

        // The core's grid: the mesh's spacing ÷ 1.5 over the Mercator box, at most 576 a side.
        let grid = Terrain3DGridRule.coreGridSize(rect: sample.mercatorRect, nodeSpacingMercator: sample.nodeSpacingMercator)
        XCTAssertGreaterThan(grid.cols, sample.cols)
        XCTAssertLessThanOrEqual(grid.cols, Terrain3DLimits.maxGridNodes)
        XCTAssertLessThanOrEqual(grid.rows, Terrain3DLimits.maxGridNodes)
        XCTAssertEqual(Terrain3DGridRule.nodeCount(spanMercator: .infinity), Terrain3DLimits.minNodes)
        XCTAssertEqual(Terrain3DGridRule.nodeCount(spanMercator: 1e12), Terrain3DLimits.maxNodes)
    }

    // MARK: - Texture coordinates

    /// The map is draped exactly: a coordinate's texture coordinate is its map pixel (through the
    /// homography's inverse and the warp) ÷ the enlarged size, so the sheet's corners land on the
    /// sheet's corners in the texture, warp and rotation included.
    func testTextureCoordinatesOfTheSheetCornersThroughAWarp() throws {
        let georef = Self.georef(rotation: 7 * .pi / 180, warp: Self.warp())
        let margin = 1000.0
        let area = try XCTUnwrap(Terrain3DArea(georef: georef, sheetWidth: Self.sheetWidth,
                                               sheetHeight: Self.sheetHeight, margin: margin))
        let w = Double(Self.sheetWidth)
        let h = Double(Self.sheetHeight)
        let ew = w + 2 * margin
        let eh = h + 2 * margin
        for corner in [SIMD2<Double>(0, 0), SIMD2(w, 0), SIMD2(w, h), SIMD2(0, h), SIMD2(w / 3, h * 0.7)] {
            // Where the georeference puts the corner on Earth (homography + warp)...
            let truth = georef.latLon(forPixel: CGPoint(x: corner.x, y: corner.y))
            // ...comes back to the corner's texel.
            let uv = area.uv(for: truth)
            XCTAssertEqual(uv.x, (corner.x + margin) / ew, accuracy: 1e-8, "\(corner)")
            XCTAssertEqual(uv.y, (corner.y + margin) / eh, accuracy: 1e-8, "\(corner)")
        }
        // The warp matters: the homography alone puts the corner elsewhere.
        let topLeft = georef.latLon(forPixel: .zero)
        let flat = georef.pixelToMercator.inverse?.apply(WebMercator.project(topLeft)) ?? SIMD2(0, 0)
        XCTAssertGreaterThan((flat.x * flat.x + flat.y * flat.y).squareRoot(), 3)

        // The sheet's rectangle in the texture is where those texture coordinates point.
        let rect = area.sheetRect(textureWidth: 2000, height: 3000)
        XCTAssertEqual(Double(rect.minX) / 2000, margin / ew, accuracy: 1e-9)
        XCTAssertEqual(Double(rect.maxY) / 3000, (h + margin) / eh, accuracy: 1e-9)
    }

    /// Each vertex shows the map pixel it stands for: its texture coordinate is the node's pixel,
    /// and its position on the ground goes back to that pixel through the inverse.
    func testMeshVerticesShowTheirOwnMapPixel() throws {
        let georef = Self.georef(rotation: -12 * .pi / 180, warp: Self.warp())
        let area = try XCTUnwrap(Terrain3DArea(georef: georef, sheetWidth: Self.sheetWidth,
                                               sheetHeight: Self.sheetHeight, margin: 1500))
        let nodes = area.localNodes()
        XCTAssertEqual(nodes.count, area.cols * area.rows)
        let mesh = Terrain3DMesh.build(cols: area.cols, rows: area.rows, nodes: nodes,
                                       heights: [Float](repeating: 0, count: nodes.count), baseM: 0)
        let inverse = georef.mercatorToPixel
        for (i, j) in [(0, 0), (17, 33), (area.cols - 1, area.rows - 1), (area.cols / 2, 5)] {
            let v = j * area.cols + i
            let pixel = area.pixel(col: Double(i), row: Double(j))
            let uv = area.uv(pixel: pixel)
            XCTAssertEqual(Double(mesh.uvs[2 * v]), uv.x, accuracy: 1e-6)
            XCTAssertEqual(Double(mesh.uvs[2 * v + 1]), uv.y, accuracy: 1e-6)
            let back = georef.pixel(forMercator: area.mercator(local: nodes[v]), inverse: inverse)
            XCTAssertEqual(back.x, pixel.x, accuracy: 1e-5, "node (\(i), \(j))")
            XCTAssertEqual(back.y, pixel.y, accuracy: 1e-5, "node (\(i), \(j))")
        }
        XCTAssertEqual(mesh.uvs[0], 0)
        XCTAssertEqual(mesh.uvs[mesh.uvs.count - 1], 1, accuracy: 1e-6)
        // x east, z south: the sheet's top-left is north-west of its centre.
        XCTAssertLessThan(nodes[0].y, 0)
    }

    // MARK: - Heights

    func testHeightsAreResampledFromTheCoreGridAndGapsFilled() throws {
        let area = try XCTUnwrap(Terrain3DArea(georef: Self.georef(rotation: 0.2), sheetWidth: Self.sheetWidth,
                                               sheetHeight: Self.sheetHeight, margin: 600))
        var grid = Self.planeGrid(area.mercatorRect)
        let heights = Terrain3DHeights.meshHeights(area: area, grid: grid)
        for (i, j) in [(0, 0), (3, 40), (area.cols - 1, area.rows - 1), (area.cols / 2, area.rows / 3)] {
            let m = area.georef.mercator(forPixel: area.pixel(col: Double(i), row: Double(j)))
            XCTAssertEqual(Double(heights[j * area.cols + i]), Self.plane(m, area.mercatorRect), accuracy: 0.01)
        }
        XCTAssertTrue(heights.allSatisfy(\.isFinite))

        // A hole in the terrain: unknown under it, filled from around it.
        for j in 10..<30 {
            for i in 8..<25 {
                grid.heights[j * grid.cols + i] = .nan
            }
        }
        var holed = Terrain3DHeights.meshHeights(area: area, grid: grid)
        let known = holed.filter(\.isFinite)
        XCTAssertLessThan(known.count, holed.count)
        let share = Terrain3DHeights.fillGaps(&holed, cols: area.cols, rows: area.rows)
        XCTAssertEqual(share, Double(holed.count - known.count) / Double(holed.count), accuracy: 1e-12)
        XCTAssertTrue(holed.allSatisfy(\.isFinite))
        let low = known.min() ?? 0
        let high = known.max() ?? 0
        XCTAssertTrue(holed.allSatisfy { $0 >= low - 0.01 && $0 <= high + 0.01 })

        // Nothing known: flat at 0.
        var none = Terrain3DHeights.meshHeights(area: area, grid: nil)
        XCTAssertTrue(none.allSatisfy(\.isNaN))
        XCTAssertEqual(Terrain3DHeights.fillGaps(&none, cols: area.cols, rows: area.rows), 1)
        XCTAssertTrue(none.allSatisfy { $0 == 0 })
    }

    func testGridSampleLeavesUnknownNodesOut() {
        let rect = MercatorRect(minX: 0, minY: 0, maxX: 100, maxY: 100)
        // Row 0 is the north edge (y = 100).
        let grid = HeightGrid(rect: rect, cols: 2, rows: 2, heights: [10, .nan, 30, 50], zoom: 12, minM: nil, maxM: nil,
                              missingFraction: 0.25)
        // Centre: the three known nodes, equal weights.
        XCTAssertEqual(Terrain3DHeights.sample(grid, at: SIMD2(50, 50)), 30, accuracy: 1e-5)
        // On the unknown node: unknown.
        XCTAssertTrue(Terrain3DHeights.sample(grid, at: SIMD2(100, 100)).isNaN)
        XCTAssertEqual(Terrain3DHeights.sample(grid, at: SIMD2(0, 0)), 30, accuracy: 1e-5)
        XCTAssertTrue(Terrain3DHeights.sample(grid, at: SIMD2(150, 50)).isNaN)
    }

    // MARK: - Mesh

    func testMeshTrianglesFaceUpWithSmoothNormalsAndSides() {
        let flat = Self.lattice { _, _ in 120 }
        let mesh = Terrain3DMesh.build(cols: 5, rows: 4, nodes: flat.nodes, heights: flat.heights, baseM: 100)
        XCTAssertEqual(mesh.vertexCount, 20)
        XCTAssertEqual(mesh.triangleCount, 2 * 4 * 3)
        // Heights above the base.
        XCTAssertEqual(mesh.positions[1], 20)
        // Counter-clockwise seen from above as built (x east, z south): front faces up.
        var t = 0
        while t + 2 < mesh.indices.count {
            let p = (0..<3).map { k -> SIMD3<Double> in
                let v = Int(mesh.indices[t + k])
                return SIMD3(Double(mesh.positions[3 * v]), Double(mesh.positions[3 * v + 1]), Double(mesh.positions[3 * v + 2]))
            }
            let a = p[1] - p[0]
            let b = p[2] - p[0]
            XCTAssertGreaterThan(a.z * b.x - a.x * b.z, 0, "triangle \(t / 3)")
            t += 3
        }
        for k in 0..<mesh.vertexCount {
            XCTAssertEqual(mesh.normals[3 * k + 1], 1, accuracy: 1e-6)
        }
        XCTAssertEqual(Array(mesh.uvs.prefix(2)), [0, 0])
        XCTAssertEqual(Array(mesh.uvs.suffix(2)), [1, 1])

        // A slope rising 0.1 m per metre east: normals lean west.
        let slope = Self.lattice { i, _ in Float(i) * 10 }
        let tilted = Terrain3DMesh.build(cols: 5, rows: 4, nodes: slope.nodes, heights: slope.heights, baseM: 0)
        let n = SIMD3<Double>(-0.1, 1, 0) / (1.01).squareRoot()
        XCTAssertEqual(Double(tilted.normals[3 * 7]), n.x, accuracy: 1e-5)
        XCTAssertEqual(Double(tilted.normals[3 * 7 + 1]), n.y, accuracy: 1e-5)

        // The sides: every edge node with one 50 m below the base.
        let ring = 2 * (5 + 4) - 4
        XCTAssertEqual(mesh.skirtPositions.count, ring * 2 * 3)
        XCTAssertEqual(mesh.skirtIndices.count, ring * 6)
        XCTAssertEqual(mesh.skirtPositions[4], Float(-Terrain3DLimits.skirtDepthM))
        XCTAssertEqual(mesh.skirtShade.count, ring * 2)

        // The coarse level keeps every second node and the last.
        XCTAssertEqual(Terrain3DMesh.kept(5, step: 2), [0, 2, 4])
        XCTAssertEqual(Terrain3DMesh.kept(4, step: 2), [0, 2, 3])
        let coarse = Terrain3DMesh.build(cols: 5, rows: 4, nodes: flat.nodes, heights: flat.heights, baseM: 100, step: 2)
        XCTAssertEqual(coarse.vertexCount, 9)
        XCTAssertEqual(coarse.triangleCount, 8)
        XCTAssertEqual(Array(coarse.uvs.suffix(2)), [1, 1])
    }

    // MARK: - Draping

    func testSurfacePointsLieOnTheMeshTriangles() {
        let rough = Self.lattice { i, j in Float(((i * 7 + j * 13) % 5) * 10) }
        let surface = Terrain3DSurface(cols: 5, rows: 4, nodes: rough.nodes, heights: rough.heights)
        // Cell (1, 1), above its diagonal: nodes (1,1) = 0 m, (2,1) = 20 m, (2,2) = 0 m.
        let p = surface.point(at: SIMD2(1.7, 1.2))
        XCTAssertEqual(p?.x ?? 0, 170, accuracy: 1e-9)
        XCTAssertEqual(p?.y ?? 0, 0.7 * 20 + 0.2 * (0 - 20), accuracy: 1e-9)
        XCTAssertEqual(p?.z ?? 0, 120, accuracy: 1e-9)
        XCTAssertEqual(surface.point(at: SIMD2(3, 2))?.y ?? -1, Double(rough.heights[2 * 5 + 3]), accuracy: 1e-9)
        XCTAssertNil(surface.point(at: SIMD2(4.5, 1)))
        XCTAssertNil(surface.point(at: SIMD2(-0.1, 1)))
        // The affine fit back from the ground, and a ground offset in grid units.
        let g = surface.gridPoint(local: SIMD2(250, 130))
        XCTAssertEqual(g.x, 2.5, accuracy: 1e-9)
        XCTAssertEqual(g.y, 1.3, accuracy: 1e-9)
        let d = surface.gridOffset(local: SIMD2(-50, 20), near: SIMD2(1.2, 2.2))
        XCTAssertEqual(d.x, -0.5, accuracy: 1e-9)
        XCTAssertEqual(d.y, 0.2, accuracy: 1e-9)
    }

    /// A line on the block is split where it crosses the triangles' edges, so it lies on the
    /// surface everywhere, not only at its points.
    func testDrapedLineFollowsTheSurfaceBetweenItsPoints() {
        let plane = Self.lattice { i, j in Float(Double(i) * 100 * 0.05 + Double(j) * 100 * 0.02) }
        let flat = Terrain3DSurface(cols: 5, rows: 4, nodes: plane.nodes, heights: plane.heights)
        let pieces = flat.drape([SIMD2(0.5, 0.5), SIMD2(3.5, 2.5)])
        XCTAssertEqual(pieces.count, 1)
        // Both ends and five crossings: columns 1, 2, 3 and rows 1, 2 (no diagonal between).
        XCTAssertEqual(pieces.first?.count, 7)
        for point in pieces.first ?? [] {
            XCTAssertEqual(point.local.y, 0.05 * point.local.x + 0.02 * point.local.z, accuracy: 1e-9)
        }
        XCTAssertEqual(flat.crossings(SIMD2(0.5, 0.5), SIMD2(3.5, 2.5)).count, 5)

        // On rough terrain, the middle of every piece between two samples is on the surface too.
        let rough = Self.lattice { i, j in Float(((i * 7 + j * 13) % 5) * 10) }
        let surface = Terrain3DSurface(cols: 5, rows: 4, nodes: rough.nodes, heights: rough.heights)
        let line = surface.drape([SIMD2(0.3, 0.2), SIMD2(3.9, 2.7), SIMD2(0.2, 2.9)])
        XCTAssertEqual(line.count, 1)
        var samples = 0
        for piece in line {
            samples += piece.count
            for (a, b) in zip(piece, piece.dropFirst()) {
                let middle = surface.point(at: (a.grid + b.grid) / 2)
                let chord = (a.local + b.local) / 2
                XCTAssertEqual(middle?.y ?? .nan, chord.y, accuracy: 1e-9)
            }
        }
        XCTAssertGreaterThan(samples, 3)

        // Off the block: clipped at its edge; leaving and coming back makes two pieces.
        XCTAssertEqual(flat.drape([SIMD2(-1, 1), SIMD2(2, 1)]).first?.first?.grid.x ?? -1, 0, accuracy: 1e-12)
        let twice = flat.drape([SIMD2(1, 1), SIMD2(5, 1), SIMD2(1, 2)])
        XCTAssertEqual(twice.count, 2)
        XCTAssertEqual(twice.last?.first?.grid.x ?? -1, 4, accuracy: 1e-12)
        XCTAssertEqual(twice.last?.first?.grid.y ?? -1, 1.25, accuracy: 1e-12)
        XCTAssertTrue(flat.drape([SIMD2(-3, -3), SIMD2(-1, -2)]).isEmpty)
        XCTAssertTrue(flat.drape([SIMD2(1, 1), SIMD2(.nan, 2)]).isEmpty)
    }

    /// A route over the real pipeline: coordinates → map pixels (inverse with the warp) → grid →
    /// draped on heights from a core-style grid. Every sample's height is the terrain's height at
    /// its place, and its ribbon floats `lift` above the terrain, exaggerated.
    func testDrapedRouteHeightsAndRibbonOverTerrain() throws {
        let georef = Self.georef(rotation: 0.15)
        let area = try XCTUnwrap(Terrain3DArea(georef: georef, sheetWidth: Self.sheetWidth,
                                               sheetHeight: Self.sheetHeight, margin: 800))
        var heights = Terrain3DHeights.meshHeights(area: area, grid: Self.planeGrid(area.mercatorRect))
        Terrain3DHeights.fillGaps(&heights, cols: area.cols, rows: area.rows)
        let base = heights.min() ?? 0
        let nodes = area.localNodes()
        let surface = Terrain3DSurface(cols: area.cols, rows: area.rows, nodes: nodes, heights: heights.map { $0 - base })
        let route = [georef.latLon(forPixel: CGPoint(x: 400, y: 600)), georef.latLon(forPixel: CGPoint(x: 2900, y: 1800)),
                     georef.latLon(forPixel: CGPoint(x: 1200, y: 4500))]
        let pieces = surface.drape(route.map { area.gridPoint(for: $0) })
        XCTAssertEqual(pieces.count, 1)
        let piece = try XCTUnwrap(pieces.first)
        XCTAssertGreaterThan(piece.count, 100, "split along the way, not only at its three points")
        for point in piece {
            let m = area.mercator(local: SIMD2(point.local.x, point.local.z))
            XCTAssertEqual(point.local.y + Double(base), Self.plane(m, area.mercatorRect), accuracy: 0.05)
        }

        let exaggeration = 2.0
        let lift = 3.0
        let ribbon = Terrain3DLines.ribbon(piece, surface: surface, width: 40, lift: lift, exaggeration: exaggeration)
        XCTAssertEqual(ribbon.positions.count, piece.count * 2 * 3)
        XCTAssertEqual(ribbon.indices.count, (piece.count - 1) * 6)
        for v in stride(from: 0, to: ribbon.positions.count / 3, by: 7) {
            let x = Double(ribbon.positions[3 * v])
            let y = Double(ribbon.positions[3 * v + 1])
            let z = Double(ribbon.positions[3 * v + 2])
            let terrain = Self.plane(area.mercator(local: SIMD2(x, z)), area.mercatorRect) - Double(base)
            XCTAssertEqual(y, terrain * exaggeration + lift, accuracy: 0.2, "vertex \(v)")
        }
        // 40 m wide.
        let l = SIMD2(Double(ribbon.positions[0]), Double(ribbon.positions[2]))
        let r = SIMD2(Double(ribbon.positions[3]), Double(ribbon.positions[5]))
        XCTAssertEqual(((l - r) * (l - r)).sum().squareRoot(), 40, accuracy: 1e-3)

        XCTAssertEqual(Terrain3DLines.lift(cameraDistance: 100), Terrain3DLimits.lineLiftM)
        XCTAssertEqual(Terrain3DLines.lift(cameraDistance: 20_000), 30, accuracy: 1e-9)
    }

    func testSimplifyKeepsEndsAndCorners() {
        let straight = (0...100).map { SIMD2(Double($0) * 0.1, Double($0) * 0.05) }
        XCTAssertEqual(Terrain3DLines.simplify(straight, tolerance: 0.2), [straight[0], straight[100]])
        let corner = [SIMD2<Double>(0, 0), SIMD2(1, 0.01), SIMD2(2, 0), SIMD2(2, 1), SIMD2(2.01, 2), SIMD2(2, 3)]
        XCTAssertEqual(Terrain3DLines.simplify(corner, tolerance: 0.2), [SIMD2(0, 0), SIMD2(2, 0), SIMD2(2, 3)])
        XCTAssertEqual(Terrain3DLines.simplify([SIMD2(0, 0), SIMD2(1, 1)], tolerance: 1).count, 2)
    }

    // MARK: - Texture size

    func testTextureSizeFollowsMemoryAndTheMapsResolution() {
        let t = Terrain3DTextureRule.size(enlargedWidth: 8467, enlargedHeight: 9920, maxSide: 4096)
        XCTAssertEqual(t.width, 3496)
        XCTAssertEqual(t.height, 4096)
        // Never more texels than map pixels.
        let small = Terrain3DTextureRule.size(enlargedWidth: 800, enlargedHeight: 600, maxSide: 4096)
        XCTAssertEqual(small.width, 800)
        XCTAssertEqual(small.height, 600)
        XCTAssertEqual(Terrain3DLimits.textureSide(physicalMemory: 8 << 30), 4096)
        XCTAssertEqual(Terrain3DLimits.textureSide(physicalMemory: 4 << 30), 3072)
        XCTAssertEqual(Terrain3DLimits.textureSide(physicalMemory: 3 << 30), 2048)
        // The core's render zoom: the coarsest whose cell is at most two texels, within the cap.
        XCTAssertEqual(Terrain3DTextureRule.renderZoom(texelGroundM: 2.42, groundScale: 0.9, maxZoom: 13), 13)
        XCTAssertEqual(Terrain3DTextureRule.renderZoom(texelGroundM: 50, groundScale: 0.9, maxZoom: 13), 11)
        XCTAssertEqual(Terrain3DTextureRule.renderZoom(texelGroundM: 50, groundScale: 0.9, maxZoom: 10), 10)
        XCTAssertEqual(Terrain3DTextureRule.sheetLongSide(sheetTexels: 2048), 3072)
        XCTAssertEqual(Terrain3DTextureRule.sheetLongSide(sheetTexels: 4000), Terrain3DLimits.maxSheetRasterSide)
    }

    // MARK: - Camera

    func testOrbitCameraLooksAlongItsHeadingFromAbove() {
        let north = Terrain3DOrbit(target: SIMD3(0, 0, 0), yaw: 0, pitch: .pi / 4, distance: 1000)
        // Looking north from the south, above.
        XCTAssertEqual(north.position.x, 0, accuracy: 1e-9)
        XCTAssertEqual(north.position.y, 1000 / 2.0.squareRoot(), accuracy: 1e-9)
        XCTAssertEqual(north.position.z, 1000 / 2.0.squareRoot(), accuracy: 1e-9)
        XCTAssertEqual(north.headingDegrees, 0, accuracy: 1e-9)
        var east = north
        east.yaw = .pi / 2
        // Looking east from the west; its right is south.
        XCTAssertEqual(east.position.x, -1000 / 2.0.squareRoot(), accuracy: 1e-9)
        XCTAssertEqual(east.position.z, 0, accuracy: 1e-9)
        XCTAssertEqual(east.groundRight.y, 1, accuracy: 1e-12)
        XCTAssertEqual(east.headingDegrees, 90, accuracy: 1e-9)
        east.yaw = -.pi / 2
        XCTAssertEqual(east.headingDegrees, 270, accuracy: 1e-9)

        // Turning the short way round, zooming evenly.
        let a = Terrain3DOrbit(target: SIMD3(0, 0, 0), yaw: 350 * .pi / 180, pitch: 0.5, distance: 1000)
        let b = Terrain3DOrbit(target: SIMD3(100, 0, 0), yaw: 10 * .pi / 180, pitch: 0.7, distance: 4000)
        let half = a.interpolated(to: b, 0.5)
        XCTAssertEqual(MapAngle.normalizedDegrees(half.headingDegrees), 0, accuracy: 1e-6)
        XCTAssertEqual(half.distance, 2000, accuracy: 1e-6)
        XCTAssertEqual(half.target.x, 50, accuracy: 1e-9)
        XCTAssertEqual(a.interpolated(to: b, 1).distance, 4000, accuracy: 1e-6)

        // Limits: not below 12°, not flat overhead, not closer than 250 m.
        let wild = Terrain3DOrbit(target: SIMD3(0, 0, 0), yaw: 0, pitch: -1, distance: 5).clamped(maxDistance: 50_000)
        XCTAssertEqual(wild.pitch, Terrain3DOrbit.minPitch, accuracy: 1e-12)
        XCTAssertEqual(wild.distance, Terrain3DOrbit.minDistance)
        XCTAssertEqual(Terrain3DOrbit(target: SIMD3(0, 0, 0), yaw: 0, pitch: 3, distance: 1e9)
            .clamped(maxDistance: 50_000).distance, 50_000)

        // Billboards keep their size: metres per point grow with distance.
        XCTAssertEqual(Terrain3DOrbit.metersPerPoint(distance: 1000, fovDegrees: 60, viewPoints: 400),
                       2 * 1000 * tan(.pi / 6) / 400, accuracy: 1e-9)
        XCTAssertGreaterThan(Terrain3DOrbit.fitDistance(extent: 9452, fovDegrees: 55), 9452 / 2 / tan(27.5 * .pi / 180))
    }

    // MARK: - Geo items

    func testOtherMapsPlacesAreNotShownTwice() {
        let peak = LatLon(lat: -25.85, lon: 27.40)
        let shown = [Terrain3DMarker(id: "a", name: "Nooitgedacht", position: peak, style: .poi(.peak), priority: 6)]
        XCTAssertTrue(Terrain3DItems.isDuplicate(" nooitgedacht", at: Geodesy.destination(peak, bearing: 40, distance: 120),
                                                 in: shown))
        XCTAssertFalse(Terrain3DItems.isDuplicate("Nooitgedacht", at: Geodesy.destination(peak, bearing: 40, distance: 400),
                                                  in: shown))
        XCTAssertFalse(Terrain3DItems.isDuplicate("Hamerkop", at: peak, in: shown))
        XCTAssertGreaterThan(Terrain3DItems.priority(.peak), Terrain3DItems.priority(.spotHeight))
    }
}
