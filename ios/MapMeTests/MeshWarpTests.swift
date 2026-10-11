import CoreGraphics
import XCTest
@testable import MapMe

/// The photo-mode warp (docs/ARCHITECTURE.md, "warp"): bilinear offsets on a lattice added to the
/// homography, `merc(p) = H(p) + B(p)`, its inverse by fixed-point iteration, storage, decoding
/// from the core, and how it follows a change of flattening.
final class MeshWarpTests: XCTestCase {
    /// 3 × 2 nodes over a 200 × 100 image (spacing 100 px), with offsets that are not linear in i.
    private let small = MeshWarp(cols: 3, rows: 2, width: 200, height: 100, offsets: MeshWarpTests.smallOffsets)

    private static func smallNode(_ i: Int, _ j: Int) -> SIMD2<Double> {
        let di = Double(i)
        let dj = Double(j)
        return SIMD2(10 * di + 100 * dj + 1, -5 * di * di + 7 * dj)
    }

    private static var smallOffsets: [Double] {
        var values: [Double] = []
        for j in 0..<2 {
            for i in 0..<3 {
                let n = smallNode(i, j)
                values.append(n.x)
                values.append(n.y)
            }
        }
        return values
    }

    /// Bilinear interpolation of the small warp's nodes, written out independently of `MeshWarp`.
    private func expectedSmall(i0: Int, j0: Int, fu: Double, fv: Double) -> SIMD2<Double> {
        let n00 = Self.smallNode(i0, j0)
        let n10 = Self.smallNode(i0 + 1, j0)
        let n01 = Self.smallNode(i0, j0 + 1)
        let n11 = Self.smallNode(i0 + 1, j0 + 1)
        let a = n00 * ((1 - fu) * (1 - fv))
        let b = n10 * (fu * (1 - fv))
        let c = n01 * ((1 - fu) * fv)
        let d = n11 * (fu * fv)
        return a + b + c + d
    }

    private func meanOfSmallNodes(_ nodes: [(Int, Int)]) -> SIMD2<Double> {
        var sum = SIMD2<Double>(0, 0)
        for (i, j) in nodes {
            sum += Self.smallNode(i, j)
        }
        return sum / Double(nodes.count)
    }

    private func assertClose(_ a: SIMD2<Double>, _ b: SIMD2<Double>, _ tolerance: Double, _ message: String = "",
                             file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(a.x, b.x, accuracy: tolerance, message, file: file, line: line)
        XCTAssertEqual(a.y, b.y, accuracy: tolerance, message, file: file, line: line)
    }

    // MARK: - Bilinear interpolation

    func testLatticeLayout() {
        XCTAssertTrue(small.isValid)
        XCTAssertEqual(small.origin, SIMD2<Double>(0, 0))
        XCTAssertEqual(small.spacing, SIMD2<Double>(100, 100))
        XCTAssertEqual(small.nodePixel(2, 1), SIMD2<Double>(200, 100))
        XCTAssertEqual(small.nodeOffset(1, 1), Self.smallNode(1, 1))
        // offsets_m[2·(j·cols + i)] is node (i, j)'s x offset.
        XCTAssertEqual(small.offsets[2 * (1 * 3 + 2)], Self.smallNode(2, 1).x)
        XCTAssertEqual(small.offsets[2 * (1 * 3 + 2) + 1], Self.smallNode(2, 1).y)
    }

    func testBilinearValuesAtNodesAndCellCentres() {
        for j in 0..<2 {
            for i in 0..<3 {
                let p = CGPoint(x: Double(i) * 100, y: Double(j) * 100)
                assertClose(small.offset(atPixel: p), Self.smallNode(i, j), 1e-12, "node (\(i), \(j))")
            }
        }
        // Cell centres: the mean of the four corners.
        assertClose(small.offset(atPixel: CGPoint(x: 50, y: 50)), meanOfSmallNodes([(0, 0), (1, 0), (0, 1), (1, 1)]), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 150, y: 50)), meanOfSmallNodes([(1, 0), (2, 0), (1, 1), (2, 1)]), 1e-12)
        // Anywhere else in a cell.
        assertClose(small.offset(atPixel: CGPoint(x: 125, y: 75)), expectedSmall(i0: 1, j0: 0, fu: 0.25, fv: 0.75), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 30, y: 10)), expectedSmall(i0: 0, j0: 0, fu: 0.3, fv: 0.1), 1e-12)
        // Edge midpoints: the mean of the two nodes.
        assertClose(small.offset(atPixel: CGPoint(x: 200, y: 50)), meanOfSmallNodes([(2, 0), (2, 1)]), 1e-12)
        // The vector entry point agrees.
        assertClose(small.offset(at: SIMD2(125, 75)), small.offset(atPixel: CGPoint(x: 125, y: 75)), 0)
    }

    func testClampsOutsideTheLattice() {
        assertClose(small.offset(atPixel: CGPoint(x: -30, y: -40)), Self.smallNode(0, 0), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 250, y: 300)), Self.smallNode(2, 1), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: -10, y: 150)), Self.smallNode(0, 1), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 900, y: 20)), expectedSmall(i0: 1, j0: 0, fu: 1, fv: 0.2), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 50, y: -1000)), small.offset(atPixel: CGPoint(x: 50, y: 0)), 1e-12)
        assertClose(small.offset(atPixel: CGPoint(x: 1e9, y: -1e9)), Self.smallNode(2, 0), 1e-12)
    }

    func testDegenerateAndMalformedWarps() {
        // A single node: a constant offset.
        let constant = MeshWarp(cols: 1, rows: 1, width: 100, height: 100, offsets: [3, -4])
        XCTAssertTrue(constant.isValid)
        assertClose(constant.offset(atPixel: CGPoint(x: 70, y: 20)), SIMD2(3, -4), 0)
        // One offset short, no nodes, zero spacing, non-finite values: not valid, zero offset.
        let short = MeshWarp(cols: 3, rows: 2, width: 200, height: 100, offsets: Array(Self.smallOffsets.dropLast()))
        XCTAssertFalse(short.isValid)
        assertClose(short.offset(atPixel: CGPoint(x: 50, y: 50)), SIMD2(0, 0), 0)
        XCTAssertFalse(MeshWarp(cols: 0, rows: 0, width: 10, height: 10, offsets: []).isValid)
        let flat = MeshWarp(cols: 2, rows: 2, origin: SIMD2(0, 0), spacing: SIMD2(0, 10), offsets: [Double](repeating: 1, count: 8))
        XCTAssertFalse(flat.isValid)
        assertClose(flat.offset(atPixel: CGPoint(x: 5, y: 5)), SIMD2(0, 0), 0)
        var poisoned = small
        poisoned.offsets[3] = Double.nan
        XCTAssertFalse(poisoned.isValid)
        // A non-finite pixel gets no offset.
        assertClose(small.offset(atPixel: CGPoint(x: Double.nan, y: 5)), SIMD2(0, 0), 0)
        // The largest node offset is node (2, 1)'s (121, −13).
        XCTAssertEqual(small.maxOffset, (121.0 * 121.0 + 13.0 * 13.0).squareRoot(), accuracy: 1e-9)
    }

    // MARK: - The full model

    /// 2 ground metres per pixel at Grootkloof with a little rotation and perspective.
    private func grootkloofHomography() -> Homography {
        let origin = WebMercator.project(LatLon(lat: -25.85, lon: 27.40))
        let s = 2 / cos(25.85 * Double.pi / 180)
        let local = Homography([s, 0.02 * s, 0, 0.01 * s, -s, 0, 1e-6, -2e-6, 1])
        let shift = Homography([1, 0, origin.x, 0, 1, origin.y, 0, 0, 1])
        return PhotoGeometry.compose(shift, local)
    }

    /// A folded sheet: offsets of up to ±80 m per axis varying smoothly over a `width` × `height`
    /// image, on a lattice like the core's.
    private func foldedWarp(width: Double, height: Double, cols: Int = 17, rows: Int = 23) -> MeshWarp {
        var offsets: [Double] = []
        for j in 0..<rows {
            for i in 0..<cols {
                let u = Double(i) / Double(cols - 1)
                let v = Double(j) / Double(rows - 1)
                let dx = 80 * sin(2 * Double.pi * u) * cos(Double.pi * v)
                let dy = 80 * cos(1.5 * Double.pi * u) * sin(2 * Double.pi * v)
                offsets.append(dx)
                offsets.append(dy)
            }
        }
        return MeshWarp(cols: cols, rows: rows, width: width, height: height, offsets: offsets)
    }

    func testForwardAndInverseRoundTripWithARealisticWarp() throws {
        let h = grootkloofHomography()
        let warp = foldedWarp(width: 3000, height: 4200)
        XCTAssertTrue(warp.isValid)
        let g = Georeference(pixelToMercator: h, method: .contours, confidence: 0.8, rmsMeters: 20, warp: warp)
        let flat = Georeference(pixelToMercator: h, method: .contours, confidence: 0.8, rmsMeters: 20)
        let projector = Georeference.Projector(g)
        var worst = 0.0
        var largestShift = 0.0
        for x in stride(from: -200.0, through: 3200, by: 170) {
            for y in stride(from: -200.0, through: 4400, by: 230) {
                let p = CGPoint(x: x, y: y)
                // Forward: H(p) + B(p).
                let m = g.mercator(forPixel: SIMD2(x, y))
                assertClose(m, h.apply(SIMD2(x, y)) + warp.offset(atPixel: p), 1e-6)
                let ll = g.latLon(forPixel: p)
                // Inverse by fixed-point iteration.
                let q = g.pixel(for: ll)
                worst = max(worst, hypot(Double(q.x) - x, Double(q.y) - y))
                let viaProjector = projector.pixel(for: ll)
                XCTAssertEqual(Double(viaProjector.x), Double(q.x), accuracy: 1e-9)
                XCTAssertEqual(Double(viaProjector.y), Double(q.y), accuracy: 1e-9)
                // Without the warp the same coordinate lands elsewhere.
                let plain = flat.pixel(for: ll)
                largestShift = max(largestShift, hypot(Double(plain.x) - x, Double(plain.y) - y))
            }
        }
        XCTAssertLessThan(worst, 0.05, "forward → inverse round trip off by \(worst) px")
        XCTAssertGreaterThan(largestShift, 20, "the warp should move points by tens of pixels")
    }

    func testScaleAndNorthIncludeTheWarp() {
        let origin = WebMercator.project(LatLon(lat: -25.85, lon: 27.40))
        let s = 2.0
        let h = Homography([s, 0, origin.x, 0, -s, origin.y, 0, 0, 1])
        // B(p) = (0.2·x, 0): x stretched by 10 %. Bilinear interpolation of a linear field is exact.
        var stretch: [Double] = []
        // B(p) = (0.2·y, 0): a shear; north tilts by atan(0.2 / 2).
        var shear: [Double] = []
        for j in 0..<5 {
            for i in 0..<5 {
                stretch += [0.2 * Double(i) * 250, 0]
                shear += [0.2 * Double(j) * 250, 0]
            }
        }
        let p = CGPoint(x: 400, y: 600)
        let lat = WebMercator.unproject(h.apply(SIMD2(400, 600))).lat
        let ground = WebMercator.groundPerMercator(lat: lat)
        let plain = Georeference(pixelToMercator: h, method: .grid, confidence: 1, rmsMeters: nil)
        let plainScale = plain.metersPerPixel(atPixel: p)
        XCTAssertEqual(plainScale, 2 * ground, accuracy: 0.01)
        XCTAssertEqual(plain.northAngle(atPixel: p), 0, accuracy: 1e-6)
        let stretched = Georeference(pixelToMercator: h, method: .grid, confidence: 1, rmsMeters: nil,
                                     warp: MeshWarp(cols: 5, rows: 5, width: 1000, height: 1000, offsets: stretch))
        // metersPerPixel averages the x and y steps: (2.2 + 2) / 2 against 2.
        XCTAssertEqual(stretched.metersPerPixel(atPixel: p) / plainScale, 1.05, accuracy: 1e-4)
        let sheared = Georeference(pixelToMercator: h, method: .grid, confidence: 1, rmsMeters: nil,
                                   warp: MeshWarp(cols: 5, rows: 5, width: 1000, height: 1000, offsets: shear))
        XCTAssertEqual(sheared.northAngle(atPixel: p), atan(0.1), accuracy: 1e-4)
    }

    // MARK: - Storage

    func testCodableRoundTrip() throws {
        let warp = foldedWarp(width: 3000, height: 4200, cols: 5, rows: 6)
        let g = Georeference(pixelToMercator: grootkloofHomography(), method: .contours, confidence: 0.7, rmsMeters: 25,
                             createdAt: Date(timeIntervalSince1970: 1_700_000_000), warp: warp)
        let data = try JSONEncoder().encode(g)
        let text = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(text.contains("\"offsets_m\""), text)
        let decoded = try JSONDecoder().decode(Georeference.self, from: data)
        XCTAssertEqual(decoded, g)
        XCTAssertEqual(decoded.warp, warp)
        // Inside a map document (meta.json) too.
        let doc = MapDocument(id: UUID(), name: "Folded sheet", createdAt: Date(timeIntervalSince1970: 1_700_000_000),
                              lastOpenedAt: nil, sourceFileName: "source.jpg", sourceKind: .photo, pixelWidth: 3000,
                              pixelHeight: 4200, renderDPI: nil, tileSize: 512, levels: 4, georef: g, georefMessage: nil,
                              pois: [], contourRGB: nil)
        let reloaded = try JSONDecoder().decode(MapDocument.self, from: JSONEncoder().encode(doc))
        XCTAssertEqual(reloaded, doc)
        XCTAssertEqual(reloaded.georef?.warp, warp)
        // A georeference without a warp writes no "warp" key.
        var plain = g
        plain.warp = nil
        XCTAssertFalse(String(decoding: try JSONEncoder().encode(plain), as: UTF8.self).contains("warp"))
    }

    func testOldGeoreferenceWithoutWarpDecodes() throws {
        // As written before warps existed.
        let json = #"{"pixelToMercator":{"m":[2.118,0,3046439.16,0,-2.1173,-2975246.24,0,0,1]},"method":"grid","confidence":0.97,"rmsMeters":1.1,"createdAt":700000000}"#
        let g = try JSONDecoder().decode(Georeference.self, from: Data(json.utf8))
        XCTAssertNil(g.warp)
        XCTAssertEqual(g.method, .grid)
        XCTAssertEqual(g.pixelToMercator.m[0], 2.118)
        XCTAssertEqual(g.createdAt, Date(timeIntervalSinceReferenceDate: 700_000_000))
        // The inverse is the plain inverse homography.
        let ll = g.latLon(forPixel: CGPoint(x: 878, y: 541))
        let px = g.pixel(for: ll)
        XCTAssertEqual(Double(px.x), 878, accuracy: 1e-6)
        XCTAssertEqual(Double(px.y), 541, accuracy: 1e-6)
        // Existing call sites build georeferences without a warp.
        XCTAssertNil(Georeference(pixelToMercator: g.pixelToMercator, method: .manual, confidence: 1, rmsMeters: nil).warp)
    }

    // MARK: - From the core

    private let responseHead = #"{"status": "ok", "method": "contours", "pixel_to_mercator": [2.2, 0.0, 3049000.0, 0.0, -2.2, -2978000.0, 0.0, 0.0, 1.0], "confidence": 0.8, "rms_m": 12.0, "scale_m_per_px": 1.98, "rotation_deg": 0.0, "corners": [], "need_tiles": [], "pois": [], "message": "Located.", "diagnostics": {"warp": {"source": "contours", "rms_before_m": 40.0}}, "timings_ms": {"contours": 900.0}"#

    private func decodeResponse(_ tail: String) throws -> GeorefResponse {
        try JSONDecoder().decode(GeorefResponse.self, from: Data((responseHead + tail).utf8))
    }

    func testGeorefResponseWithWarp() throws {
        let response = try decodeResponse(#", "warp": {"cols": 3, "rows": 2, "origin": [0.0, 0.0], "spacing": [100.0, 100.0], "offsets_m": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]}}"#)
        let warp = try XCTUnwrap(response.warp)
        XCTAssertEqual(warp.cols, 3)
        XCTAssertEqual(warp.rows, 2)
        XCTAssertEqual(warp.spacing, SIMD2<Double>(100, 100))
        XCTAssertEqual(warp.nodeOffset(1, 1), SIMD2<Double>(9, 10))
        let g = try XCTUnwrap(response.georeference)
        XCTAssertEqual(g.method, .contours)
        XCTAssertEqual(g.warp, warp)
        // merc(p) = H(p) + B(p).
        let p = CGPoint(x: 150, y: 50)
        // Lattice (1.5, 0.5): the mean of nodes (1, 0), (2, 0), (1, 1) and (2, 1).
        let expected = WebMercator.unproject(g.pixelToMercator.apply(SIMD2(150, 50)) + SIMD2(7, 8))
        let ll = g.latLon(forPixel: p)
        XCTAssertEqual(ll.lat, expected.lat, accuracy: 1e-12)
        XCTAssertEqual(ll.lon, expected.lon, accuracy: 1e-12)
        let back = g.pixel(for: ll)
        XCTAssertEqual(Double(back.x), 150, accuracy: 1e-6)
        XCTAssertEqual(Double(back.y), 50, accuracy: 1e-6)
        XCTAssertEqual(response.timingsMs?["contours"], 900)
    }

    func testGeorefResponseWithoutWarp() throws {
        for tail in ["}", #", "warp": null}"#] {
            let response = try decodeResponse(tail)
            XCTAssertNil(response.warp, tail)
            let g = try XCTUnwrap(response.georeference, tail)
            XCTAssertNil(g.warp, tail)
            XCTAssertEqual(g.rmsMeters, 12)
            XCTAssertEqual(response.message, "Located.")
        }
        let failed = try JSONDecoder().decode(GeorefResponse.self, from: Data(#"{"status": "failed", "message": "No match."}"#.utf8))
        XCTAssertNil(failed.warp)
        XCTAssertNil(failed.georeference)
    }

    func testMalformedWarpIsDroppedNotFatal() throws {
        // One offset short: decoded, but not trusted.
        let short = try decodeResponse(#", "warp": {"cols": 3, "rows": 2, "origin": [0, 0], "spacing": [100, 100], "offsets_m": [1, 2, 3, 4, 5]}}"#)
        XCTAssertEqual(short.warp?.isValid, false)
        let g = try XCTUnwrap(short.georeference)
        XCTAssertNil(g.warp)
        // Not even an object: the rest of the answer still decodes.
        let garbled = try decodeResponse(#", "warp": "folded"}"#)
        XCTAssertNil(garbled.warp)
        XCTAssertNotNil(garbled.georeference)
        let missingField = try decodeResponse(#", "warp": {"cols": 3, "rows": 2}}"#)
        XCTAssertNil(missingField.warp)
        XCTAssertEqual(missingField.status, "ok")
    }

    // MARK: - A change of flattening

    func testCompositionResamplesTheWarp() throws {
        // A photo and two flattenings of its page (old and new corners), as in "Adjust Corners".
        let photoToMercator = Homography([2.0, 0.1, 3_000_000, -0.05, -2.0, -2_900_000, 0, 0, 1])
        let old = PhotoQuad(topLeft: CGPoint(x: 420, y: 380), topRight: CGPoint(x: 2600, y: 520),
                            bottomRight: CGPoint(x: 2500, y: 3500), bottomLeft: CGPoint(x: 380, y: 3300))
        let new = PhotoQuad(topLeft: CGPoint(x: 400, y: 400), topRight: CGPoint(x: 2620, y: 500),
                            bottomRight: CGPoint(x: 2480, y: 3520), bottomLeft: CGPoint(x: 400, y: 3280))
        let oldWidth = 2200, oldHeight = 3000
        let newWidth = 2250, newHeight = 3100
        let oldRect = try XCTUnwrap(PhotoGeometry.rectification(quad: old, width: oldWidth, height: oldHeight))
        let newRect = try XCTUnwrap(PhotoGeometry.rectification(quad: new, width: newWidth, height: newHeight))
        let warp = foldedWarp(width: Double(oldWidth), height: Double(oldHeight))
        let oldGeoref = Georeference(pixelToMercator: PhotoGeometry.compose(photoToMercator, try XCTUnwrap(oldRect.inverse)),
                                     method: .contours, confidence: 0.8, rmsMeters: 20, warp: warp)
        let carried = try XCTUnwrap(MapImporter.carriedGeoreference(oldGeoref, oldCorners: old, oldWidth: oldWidth,
                                                                    oldHeight: oldHeight, newCorners: new,
                                                                    newWidth: newWidth, newHeight: newHeight))
        let carriedWarp = try XCTUnwrap(carried.warp, "the warp must survive a change of corners")
        XCTAssertEqual(carriedWarp.cols, warp.cols)
        XCTAssertEqual(carriedWarp.rows, warp.rows)
        XCTAssertEqual(carriedWarp.origin, SIMD2<Double>(0, 0))
        XCTAssertEqual(carriedWarp.spacing.x, Double(newWidth) / Double(warp.cols - 1), accuracy: 1e-9)
        XCTAssertEqual(carriedWarp.spacing.y, Double(newHeight) / Double(warp.rows - 1), accuracy: 1e-9)
        XCTAssertEqual(carried.method, .contours)

        // At the new lattice's nodes the carried model is exactly the old one at the same photo spot.
        let newToOld = PhotoGeometry.compose(oldRect, try XCTUnwrap(newRect.inverse))
        for j in stride(from: 0, to: carriedWarp.rows, by: 3) {
            for i in stride(from: 0, to: carriedWarp.cols, by: 3) {
                let node = carriedWarp.nodePixel(i, j)
                assertClose(carried.mercator(forPixel: node), oldGeoref.mercator(forPixel: newToOld.apply(node)), 1e-5,
                            "node (\(i), \(j))")
            }
        }
        // Between nodes, within the resampling error of a smooth ±80 m warp (~2.5 m); dropping the
        // warp would be off by up to ~80 m.
        var worst = 0.0
        var withoutWarp = 0.0
        var flatCarried = carried
        flatCarried.warp = nil
        for x in stride(from: 450.0, through: 2450, by: 100) {
            for y in stride(from: 550.0, through: 3250, by: 90) {
                let photo = CGPoint(x: x, y: y)
                let before = oldGeoref.mercator(forPixel: simd(PhotoGeometry.apply(oldRect, photo)))
                let after = carried.mercator(forPixel: simd(PhotoGeometry.apply(newRect, photo)))
                worst = max(worst, hypot(after.x - before.x, after.y - before.y))
                let flatAfter = flatCarried.mercator(forPixel: simd(PhotoGeometry.apply(newRect, photo)))
                withoutWarp = max(withoutWarp, hypot(flatAfter.x - before.x, flatAfter.y - before.y))
            }
        }
        XCTAssertLessThan(worst, 5, "carried model off by \(worst) Mercator m")
        XCTAssertGreaterThan(withoutWarp, 50)

        // No warp in, no warp out.
        var plain = oldGeoref
        plain.warp = nil
        let carriedPlain = try XCTUnwrap(MapImporter.carriedGeoreference(plain, oldCorners: old, oldWidth: oldWidth,
                                                                         oldHeight: oldHeight, newCorners: new,
                                                                         newWidth: newWidth, newHeight: newHeight))
        XCTAssertNil(carriedPlain.warp)
    }

    func testResampledThroughIdentityKeepsTheWarp() {
        let warp = foldedWarp(width: 2000, height: 3000, cols: 9, rows: 12)
        let same = warp.resampled(cols: 9, rows: 12, width: 2000, height: 3000) { $0 }
        XCTAssertEqual(same.cols, 9)
        XCTAssertEqual(same.rows, 12)
        XCTAssertEqual(same.offsets.count, warp.offsets.count)
        for (a, b) in zip(same.offsets, warp.offsets) {
            XCTAssertEqual(a, b, accuracy: 1e-9)
        }
    }

    private func simd(_ p: CGPoint) -> SIMD2<Double> { SIMD2(Double(p.x), Double(p.y)) }
}
