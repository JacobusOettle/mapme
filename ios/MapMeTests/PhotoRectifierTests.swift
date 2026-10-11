import CoreGraphics
import CoreImage
import ImageIO
import XCTest
@testable import MapMe

/// Photos of paper maps: coordinate conventions, corner ordering, the flattening math, the Core
/// Image warp, edge snapping and the stored model.
final class PhotoRectifierTests: XCTestCase {
    private let photoSize = CGSize(width: 3000, height: 4000)

    /// A sheet in perspective inside a 3000 × 4000 photo.
    private let sheet = PhotoQuad(topLeft: CGPoint(x: 420, y: 380), topRight: CGPoint(x: 2600, y: 520),
                                  bottomRight: CGPoint(x: 2500, y: 3500), bottomLeft: CGPoint(x: 380, y: 3300))

    private func assertClose(_ a: CGPoint, _ b: CGPoint, _ tolerance: Double, _ message: String = "",
                             file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertLessThanOrEqual(PhotoGeometry.distance(a, b), tolerance, "\(a) vs \(b) \(message)", file: file, line: line)
    }

    private static func permutations(_ items: [CGPoint]) -> [[CGPoint]] {
        guard items.count > 1 else { return [items] }
        var result: [[CGPoint]] = []
        for i in items.indices {
            var rest = items
            let head = rest.remove(at: i)
            for tail in permutations(rest) {
                result.append([head] + tail)
            }
        }
        return result
    }

    // MARK: - Coordinate conventions

    func testVisionAndCoreImageCoordinatesFlipY() {
        // Vision: normalised, origin bottom-left. (0.25, 0.9) is near the top of a 400 × 1000 image.
        let p = PhotoGeometry.pixel(fromVision: CGPoint(x: 0.25, y: 0.9), imageSize: CGSize(width: 400, height: 1000))
        XCTAssertEqual(p.x, 100, accuracy: 1e-9)
        XCTAssertEqual(p.y, 100, accuracy: 1e-9)
        // Core Image: pixels, origin bottom-left.
        let ci = PhotoGeometry.coreImagePoint(fromPixel: CGPoint(x: 30, y: 10), imageHeight: 200)
        XCTAssertEqual(ci.x, 30, accuracy: 1e-9)
        XCTAssertEqual(ci.y, 190, accuracy: 1e-9)
        let back = PhotoGeometry.pixel(fromCoreImage: ci, imageHeight: 200)
        XCTAssertEqual(back.x, 30, accuracy: 1e-9)
        XCTAssertEqual(back.y, 10, accuracy: 1e-9)
    }

    // MARK: - Corners

    func testOrderingStartsTopLeftAndRunsClockwise() throws {
        for shuffled in Self.permutations(sheet.points) {
            XCTAssertEqual(PhotoQuad.ordered(shuffled), sheet, "\(shuffled)")
        }
        // A page turned 30° on the table keeps its own top-left first.
        let c = CGPoint(x: 1500, y: 2000)
        let angle = 30.0 * Double.pi / 180
        let local = [CGPoint(x: -600, y: -900), CGPoint(x: 600, y: -900), CGPoint(x: 600, y: 900), CGPoint(x: -600, y: 900)]
        let turned = local.map { p -> CGPoint in
            let x = Double(p.x) * cos(angle) - Double(p.y) * sin(angle)
            let y = Double(p.x) * sin(angle) + Double(p.y) * cos(angle)
            return CGPoint(x: Double(c.x) + x, y: Double(c.y) + y)
        }
        let expected = try XCTUnwrap(PhotoQuad(points: turned))
        XCTAssertTrue(expected.isConvexClockwise)
        XCTAssertEqual(PhotoQuad.ordered(Array(turned.reversed())), expected)
        XCTAssertEqual(PhotoQuad.ordered([turned[2], turned[0], turned[3], turned[1]]), expected)
        // Not four distinct points.
        XCTAssertNil(PhotoQuad.ordered([c, c, c, c]))
        XCTAssertNil(PhotoQuad.ordered([c, c, c]))
    }

    func testConvexityAreaAnglesRotationAndJSON() throws {
        let square = PhotoQuad.full(CGSize(width: 100, height: 100))
        XCTAssertTrue(square.isConvexClockwise)
        XCTAssertEqual(square.signedArea, 10_000, accuracy: 1e-9)
        XCTAssertEqual(square.area, 10_000, accuracy: 1e-9)
        for a in square.interiorAngles {
            XCTAssertEqual(a, 90, accuracy: 1e-9)
        }
        let counterClockwise = try XCTUnwrap(PhotoQuad(points: Array(square.points.reversed())))
        XCTAssertFalse(counterClockwise.isConvexClockwise)
        XCTAssertLessThan(counterClockwise.signedArea, 0)
        let bowTie = PhotoQuad(topLeft: square.topLeft, topRight: square.bottomRight,
                               bottomRight: square.topRight, bottomLeft: square.bottomLeft)
        XCTAssertFalse(bowTie.isConvexClockwise)

        // A quarter turn keeps the outline and makes the old left edge the top.
        let turned = square.rotatedClockwise
        XCTAssertEqual(turned.topLeft, square.bottomLeft)
        XCTAssertEqual(turned.topRight, square.topLeft)
        XCTAssertTrue(turned.isConvexClockwise)
        XCTAssertEqual(turned.rotatedClockwise.rotatedClockwise.rotatedClockwise, square)

        XCTAssertEqual(PhotoQuad(json: sheet.json), sheet)
        XCTAssertNil(PhotoQuad(json: [[1, 2], [3, 4]]))
        XCTAssertNil(PhotoQuad(json: [[1, 2], [3, 4], [5, 6], [7]]))
        XCTAssertEqual(sheet.scaled(x: 0.5, y: 0.25).bottomRight, CGPoint(x: 1250, y: 875))
        let shifted = PhotoQuad.full(photoSize).mapped { p in CGPoint(x: p.x + 5000, y: p.y - 9000) }
        let corner = CGPoint(x: 3000, y: 0)
        let pinned = PhotoQuad(topLeft: corner, topRight: corner, bottomRight: corner, bottomLeft: corner)
        XCTAssertEqual(shifted.clamped(to: photoSize), pinned)
    }

    func testTriage() {
        XCTAssertEqual(PhotoGeometry.triage(nil, imageSize: photoSize), .asIs)
        // Screenshots and scans: the "page" is (almost) the whole frame.
        let frame = PhotoQuad(topLeft: CGPoint(x: 20, y: 25), topRight: CGPoint(x: 2980, y: 25),
                              bottomRight: CGPoint(x: 2980, y: 3975), bottomLeft: CGPoint(x: 20, y: 3975))
        XCTAssertEqual(PhotoGeometry.triage(frame, imageSize: photoSize), .asIs)
        // A sheet on a table in perspective goes to the corner editor.
        XCTAssertEqual(PhotoGeometry.triage(sheet, imageSize: photoSize), .needsCorners(sheet))
        // Too small, or not a clockwise convex outline: ignored.
        let tiny = PhotoQuad(topLeft: CGPoint(x: 10, y: 10), topRight: CGPoint(x: 300, y: 10),
                             bottomRight: CGPoint(x: 300, y: 300), bottomLeft: CGPoint(x: 10, y: 300))
        XCTAssertEqual(PhotoGeometry.triage(tiny, imageSize: photoSize), .asIs)
        let reversed = PhotoQuad(points: Array(sheet.points.reversed()))
        XCTAssertEqual(PhotoGeometry.triage(reversed, imageSize: photoSize), .asIs)

        // What the simulator's Vision reports for a sheet on a table: (nearly) the whole frame.
        XCTAssertTrue(PhotoGeometry.fillsFrame(frame, imageSize: photoSize))
        XCTAssertTrue(PhotoGeometry.fillsFrame(PhotoQuad.full(photoSize), imageSize: photoSize))
        XCTAssertFalse(PhotoGeometry.fillsFrame(sheet, imageSize: photoSize))

        XCTAssertTrue(PhotoGeometry.isFullImage(PhotoQuad.full(photoSize), imageSize: photoSize))
        XCTAssertFalse(PhotoGeometry.isFullImage(PhotoQuad.full(photoSize).rotatedClockwise, imageSize: photoSize))
        XCTAssertFalse(PhotoGeometry.isFullImage(sheet, imageSize: photoSize))
    }

    // MARK: - Homographies

    func testHomographyMapsCornersExactlyAndComposes() throws {
        let target = [CGPoint(x: 0, y: 0), CGPoint(x: 1000, y: 0), CGPoint(x: 1000, y: 1400), CGPoint(x: 0, y: 1400)]
        let h = try XCTUnwrap(PhotoGeometry.homography(from: sheet.points, to: target))
        for (s, t) in zip(sheet.points, target) {
            assertClose(PhotoGeometry.apply(h, s), t, 1e-6)
        }
        let rect = try XCTUnwrap(PhotoGeometry.rectification(quad: sheet, width: 1000, height: 1400))
        for (s, t) in zip(sheet.points, target) {
            assertClose(PhotoGeometry.apply(rect, s), t, 1e-6)
        }
        // compose(a, b) applies b first.
        let shift = Homography([1, 0, 10, 0, 1, 20, 0, 0, 1])
        let double = Homography([2, 0, 0, 0, 2, 0, 0, 0, 1])
        assertClose(PhotoGeometry.apply(PhotoGeometry.compose(double, shift), CGPoint(x: 1, y: 1)), CGPoint(x: 22, y: 42), 1e-12)
        let inverse = try XCTUnwrap(h.inverse)
        let p = CGPoint(x: 1234, y: 2345)
        assertClose(PhotoGeometry.apply(PhotoGeometry.compose(inverse, h), p), p, 1e-6)
        // The midpoint of the sheet's diagonals is the centre of the flattened page (perspective).
        let diagonal = intersection(sheet.topLeft, sheet.bottomRight, sheet.topRight, sheet.bottomLeft)
        assertClose(PhotoGeometry.apply(h, diagonal), CGPoint(x: 500, y: 700), 1e-6)
    }

    private func intersection(_ a1: CGPoint, _ a2: CGPoint, _ b1: CGPoint, _ b2: CGPoint) -> CGPoint {
        let d1x = Double(a2.x - a1.x), d1y = Double(a2.y - a1.y)
        let d2x = Double(b2.x - b1.x), d2y = Double(b2.y - b1.y)
        let denom = d1x * d2y - d1y * d2x
        let s = (Double(b1.x - a1.x) * d2y - Double(b1.y - a1.y) * d2x) / denom
        return CGPoint(x: Double(a1.x) + s * d1x, y: Double(a1.y) + s * d1y)
    }

    // MARK: - Flattened size

    func testPageAspectRecoveredFromPerspective() throws {
        let trueAspect = 3508.0 / 4962.0
        let poses: [(Double, Double, Double)] = [(12, -7, 2.5), (25, 10, -4), (30, 0, 0), (0, 22, 3), (3, 2, 1)]
        for (tiltX, tiltY, roll) in poses {
            var camera = SyntheticPhoto.Camera()
            camera.fill = 0.65
            camera.shiftY = -0.05
            camera.tiltX = tiltX
            camera.tiltY = tiltY
            camera.roll = roll
            let quad = SyntheticPhoto.projectedCorners(pageAspect: trueAspect, camera: camera)
            let size = CGSize(width: camera.width, height: camera.height)
            XCTAssertTrue(PhotoGeometry.isPlausible(quad, imageSize: size), "pose \(tiltX) \(tiltY) \(roll)")
            let aspect = try XCTUnwrap(PhotoGeometry.pageAspect(of: quad, imageSize: size, focalLength35mm: camera.focalLength35mm))
            XCTAssertEqual(aspect, trueAspect, accuracy: 0.002, "pose \(tiltX) \(tiltY) \(roll)")
            let out = PhotoGeometry.outputSize(for: quad, imageSize: size, focalLength35mm: camera.focalLength35mm)
            XCTAssertEqual(Double(out.width) / Double(out.height), trueAspect, accuracy: 0.003)
            // Never smaller than the sheet appears in the photo.
            let edges = quad.edgeLengths
            XCTAssertGreaterThanOrEqual(Double(out.width) + 1, max(edges.top, edges.bottom))
            XCTAssertGreaterThanOrEqual(Double(out.height) + 1, max(edges.left, edges.right))
        }
        // Without the focal length the solved one (or a typical phone lens) still lands close.
        let camera = SyntheticPhoto.Camera()
        let quad = SyntheticPhoto.projectedCorners(pageAspect: trueAspect, camera: camera)
        let aspect = try XCTUnwrap(PhotoGeometry.pageAspect(of: quad, imageSize: CGSize(width: camera.width, height: camera.height)))
        XCTAssertEqual(aspect, trueAspect, accuracy: 0.01)
    }

    func testOutputSizeOfAFlatPageAndCaps() {
        let flat = PhotoGeometry.outputSize(for: PhotoQuad.full(CGSize(width: 1200, height: 800)),
                                            imageSize: CGSize(width: 1200, height: 800))
        XCTAssertEqual(flat.width, 1200)
        XCTAssertEqual(flat.height, 800)
        let big = CGSize(width: 9000, height: 7000)
        let capped = PhotoGeometry.outputSize(for: PhotoQuad.full(big), imageSize: big)
        XCTAssertEqual(max(capped.width, capped.height), 6000)
        XCTAssertEqual(Double(capped.width) / Double(capped.height), 9.0 / 7.0, accuracy: 0.001)
        let square = CGSize(width: 8000, height: 8000)
        let limited = PhotoGeometry.outputSize(for: PhotoQuad.full(square), imageSize: square, maxLongSide: 10_000)
        XCTAssertLessThanOrEqual(Double(limited.width * limited.height), 36_000_000 * 1.001)
    }

    // MARK: - Core Image warp

    /// White page with a red block top-left and a blue block bottom-right.
    private func markerPage(width: Int = 600, height: Int = 800) throws -> CGImage {
        let ctx = try XCTUnwrap(TileStore.makeContext(width: width, height: height))
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        // CoreGraphics is y-up: the top-left block sits at the top of the bitmap.
        ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        ctx.fill(CGRect(x: 40, y: height - 40 - 140, width: 140, height: 140))
        ctx.setFillColor(CGColor(red: 0, green: 0, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: width - 40 - 140, y: 40, width: 140, height: 140))
        return try XCTUnwrap(ctx.makeImage())
    }

    // Split into small steps: long chains of Int(...) conversions and literals strain the type checker.
    private func isRed(_ p: (r: UInt8, g: UInt8, b: UInt8)) -> Bool {
        let r = Int(p.r)
        let g = Int(p.g)
        let b = Int(p.b)
        guard r > 120 else { return false }
        return r > 2 * (g + 10) && r > 2 * (b + 10)
    }

    private func isBlue(_ p: (r: UInt8, g: UInt8, b: UInt8)) -> Bool {
        let r = Int(p.r)
        let g = Int(p.g)
        let b = Int(p.b)
        guard b > 100 else { return false }
        return b > 2 * (r + 10) && b > g + 40
    }

    private func isPaper(_ p: (r: UInt8, g: UInt8, b: UInt8)) -> Bool {
        let sum = Int(p.r) + Int(p.g) + Int(p.b)
        return sum > 420
    }

    private func pixel(_ pixels: RGBAPixels, _ p: CGPoint) -> (r: UInt8, g: UInt8, b: UInt8) {
        let x = min(max(Int(p.x), 0), pixels.width - 1)
        let y = min(max(Int(p.y), 0), pixels.height - 1)
        return pixels.rgb(x, y)
    }

    func testSyntheticPhotoPutsThePageWhereTheCameraSees() throws {
        let page = try markerPage()
        var camera = SyntheticPhoto.Camera()
        camera.width = 1200
        camera.height = 1600
        camera.fill = 0.72
        let photo = try SyntheticPhoto.make(page: page, camera: camera)
        XCTAssertEqual(photo.image.width, 1200)
        XCTAssertEqual(photo.image.height, 1600)
        let pixels = try XCTUnwrap(RGBAPixels(photo.image))
        // Marker centres (page pixels, top-left origin) land where page → photo says.
        let red = PhotoGeometry.apply(photo.pageToPhoto, CGPoint(x: 110, y: 110))
        let blue = PhotoGeometry.apply(photo.pageToPhoto, CGPoint(x: 490, y: 690))
        let middle = PhotoGeometry.apply(photo.pageToPhoto, CGPoint(x: 300, y: 400))
        XCTAssertTrue(isRed(pixel(pixels, red)), "\(pixel(pixels, red)) at \(red)")
        XCTAssertTrue(isBlue(pixel(pixels, blue)), "\(pixel(pixels, blue)) at \(blue)")
        XCTAssertTrue(isPaper(pixel(pixels, middle)), "\(pixel(pixels, middle))")
        // The table around the sheet is dark.
        let corners = photo.corners.points
        let outside = CGPoint(x: corners[0].x - 30, y: corners[0].y - 30)
        let t = pixel(pixels, outside)
        XCTAssertLessThan(Int(t.r) + Int(t.g) + Int(t.b), 240, "table at \(outside): \(t)")
        // Camera EXIF is readable.
        let info = PhotoCaptureInfo.read(from: photo.jpeg)
        XCTAssertTrue(info.isCamera)
        XCTAssertEqual(try XCTUnwrap(info.focalLength35mm), 26, accuracy: 0.5)
    }

    func testFlattenUndoesThePerspective() throws {
        let page = try markerPage()
        var camera = SyntheticPhoto.Camera()
        camera.width = 1200
        camera.height = 1600
        camera.tiltX = 22
        camera.tiltY = -9
        camera.fill = 0.65
        let photo = try SyntheticPhoto.make(page: page, camera: camera)
        let quad = photo.corners
        let flat = try MapPhotoRectifier.flatten(photo.image, quad: quad, focalLength35mm: camera.focalLength35mm)
        let size = PhotoGeometry.outputSize(for: quad, imageSize: CGSize(width: 1200, height: 1600),
                                            focalLength35mm: camera.focalLength35mm)
        XCTAssertEqual(flat.width, size.width)
        XCTAssertEqual(flat.height, size.height)
        XCTAssertEqual(Double(flat.width) / Double(flat.height), 0.75, accuracy: 0.01)
        let pixels = try XCTUnwrap(RGBAPixels(flat))
        // Upright, not mirrored: red top-left, blue bottom-right, paper in the middle.
        let sx = Double(flat.width) / 600
        let sy = Double(flat.height) / 800
        XCTAssertTrue(isRed(pixel(pixels, CGPoint(x: 110 * sx, y: 110 * sy))))
        XCTAssertTrue(isBlue(pixel(pixels, CGPoint(x: 490 * sx, y: 690 * sy))))
        XCTAssertTrue(isPaper(pixel(pixels, CGPoint(x: 490 * sx, y: 110 * sy))))
        XCTAssertTrue(isPaper(pixel(pixels, CGPoint(x: 110 * sx, y: 690 * sy))))
        // The warp is exactly `rectification`: block edges are where page → photo → flat puts them.
        let rect = try XCTUnwrap(PhotoGeometry.rectification(quad: quad, width: flat.width, height: flat.height))
        let pageToFlat = PhotoGeometry.compose(rect, photo.pageToPhoto)
        let edgeInside = PhotoGeometry.apply(pageToFlat, CGPoint(x: 176, y: 110))
        let edgeOutside = PhotoGeometry.apply(pageToFlat, CGPoint(x: 186, y: 110))
        XCTAssertTrue(isRed(pixel(pixels, edgeInside)), "\(edgeInside)")
        XCTAssertTrue(isPaper(pixel(pixels, edgeOutside)), "\(edgeOutside)")
        // Bad corners are refused.
        let bowTie = PhotoQuad(topLeft: quad.topLeft, topRight: quad.bottomRight, bottomRight: quad.topRight, bottomLeft: quad.bottomLeft)
        XCTAssertThrowsError(try MapPhotoRectifier.flatten(photo.image, quad: bowTie))
    }

    // MARK: - Edge snapping

    func testEdgeSnappingFindsThePaperBorder() throws {
        // A sheet with a printed frame line just inside its edge (like a map's neatline).
        let ctx = try XCTUnwrap(TileStore.makeContext(width: 1000, height: 1414))
        ctx.setFillColor(CGColor(red: 0.97, green: 0.96, blue: 0.93, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1000, height: 1414))
        ctx.setStrokeColor(CGColor(red: 0.1, green: 0.1, blue: 0.1, alpha: 1))
        ctx.setLineWidth(2)
        ctx.stroke(CGRect(x: 30, y: 30, width: 940, height: 1354))
        let page = try XCTUnwrap(ctx.makeImage())
        var camera = SyntheticPhoto.Camera()
        camera.width = 1500
        camera.height = 2000
        camera.tiltX = 16
        camera.fill = 0.72
        let photo = try SyntheticPhoto.make(page: page, camera: camera)
        let gray = try XCTUnwrap(GrayImage(photo.image))
        XCTAssertEqual(gray.width, 1500)
        // Vision-sized errors: every corner off by up to ~2 % of the long side.
        let offsets: [(CGFloat, CGFloat)] = [(30, -25), (-35, 20), (25, 35), (-30, -30)]
        var roughPoints: [CGPoint] = []
        for (corner, offset) in zip(photo.corners.points, offsets) {
            roughPoints.append(CGPoint(x: corner.x + offset.0, y: corner.y + offset.1))
        }
        let rough = try XCTUnwrap(PhotoQuad(points: roughPoints))
        let refined = PhotoQuadDetector.refine(rough, in: gray)
        for (r, t) in zip(refined.points, photo.corners.points) {
            assertClose(r, t, 3, "refined corner")
        }
        // Nothing to snap to (a blank image): the corners stay put.
        let blank = GrayImage(width: 400, height: 400, pixels: [Float](repeating: 0.5, count: 400 * 400))
        let square = PhotoQuad(topLeft: CGPoint(x: 50, y: 50), topRight: CGPoint(x: 350, y: 60),
                               bottomRight: CGPoint(x: 340, y: 350), bottomLeft: CGPoint(x: 60, y: 340))
        XCTAssertEqual(PhotoQuadDetector.refine(square, in: blank), square)
    }

    // MARK: - Page detection: choosing between Vision and the core

    /// Pages in a 1200 × 1600 working image.
    private let workSize = CGSize(width: 1200, height: 1600)
    private let workPage = PhotoQuad(topLeft: CGPoint(x: 235, y: 171), topRight: CGPoint(x: 985, y: 249),
                                     bottomRight: CGPoint(x: 1047, y: 1357), bottomLeft: CGPoint(x: 124, y: 1370))
    /// The simulator's Vision answer for the synthetic photo: the frame, inset by a few pixels.
    private let visionFrame = PhotoQuad(topLeft: CGPoint(x: 8.3, y: 6.25), topRight: CGPoint(x: 1191.7, y: 6.25),
                                        bottomRight: CGPoint(x: 1191.7, y: 1593.75), bottomLeft: CGPoint(x: 8.3, y: 1593.75))
    /// Something else on the table (e.g. only the upper part of the sheet).
    private let otherOutline = PhotoQuad(topLeft: CGPoint(x: 235, y: 171), topRight: CGPoint(x: 985, y: 249),
                                         bottomRight: CGPoint(x: 1010, y: 760), bottomLeft: CGPoint(x: 180, y: 770))

    private func shifted(_ quad: PhotoQuad, by d: CGFloat) -> PhotoQuad {
        quad.mapped { CGPoint(x: $0.x + d, y: $0.y - d) }
    }

    func testPageSelection() {
        let near = shifted(workPage, by: 10)
        let contrasts: [(PhotoQuad, Double?)] = [(workPage, 0.6), (near, 0.58), (visionFrame, nil), (otherOutline, 0.3)]
        let contrast: (PhotoQuad) -> Double? = { quad in
            guard let entry = contrasts.first(where: { $0.0 == quad }) else { return 0 }
            return entry.1
        }
        func choose(_ vision: PhotoQuad?, _ core: PhotoQuad?) -> PageChoice? {
            PhotoQuadDetector.select(vision: vision, core: core, imageSize: workSize, contrast: contrast)
        }
        // The simulator case: Vision returns the frame, the core finds the sheet.
        XCTAssertEqual(choose(visionFrame, workPage), PageChoice(quad: workPage, source: .core))
        XCTAssertEqual(choose(nil, workPage), PageChoice(quad: workPage, source: .core))
        // Vision's frame alone is passed on (triage then uses the photo as it is).
        XCTAssertEqual(choose(visionFrame, nil), PageChoice(quad: visionFrame, source: .vision))
        XCTAssertEqual(PhotoGeometry.triage(visionFrame, imageSize: workSize), .asIs)
        // Agreeing outlines: the core's (it fits its sides itself; Vision's rest on snapping).
        XCTAssertEqual(choose(near, workPage), PageChoice(quad: workPage, source: .core))
        // Disagreeing: the clearer border, with an edge for Vision.
        XCTAssertEqual(choose(otherOutline, workPage), PageChoice(quad: workPage, source: .core))
        XCTAssertEqual(choose(workPage, otherOutline), PageChoice(quad: workPage, source: .vision))
        let closeCall: (PhotoQuad) -> Double? = { quad in quad == self.otherOutline ? 0.55 : 0.6 }
        XCTAssertEqual(PhotoQuadDetector.select(vision: otherOutline, core: workPage, imageSize: workSize, contrast: closeCall),
                       PageChoice(quad: otherOutline, source: .vision))
        // An outline with no border in the pixels is no page (a featureless image).
        XCTAssertNil(PhotoQuadDetector.select(vision: workPage, core: nil, imageSize: workSize) { _ in 0.005 })
        XCTAssertEqual(PhotoQuadDetector.select(vision: workPage, core: nil, imageSize: workSize) { _ in 0.03 },
                       PageChoice(quad: workPage, source: .vision))
        // The core's outline needs a clearer border than Vision's.
        XCTAssertNil(PhotoQuadDetector.select(vision: nil, core: workPage, imageSize: workSize) { _ in 0.04 })
        // Implausible outlines are ignored.
        let tiny = PhotoQuad(topLeft: CGPoint(x: 10, y: 10), topRight: CGPoint(x: 100, y: 10),
                             bottomRight: CGPoint(x: 100, y: 100), bottomLeft: CGPoint(x: 10, y: 100))
        XCTAssertNil(choose(tiny, nil))
        XCTAssertEqual(choose(tiny, workPage), PageChoice(quad: workPage, source: .core))
        XCTAssertNil(choose(nil, nil))

        // The core's answer counts when it is confident and does not fill the frame; its corners
        // are put in TL, TR, BR, BL order.
        let coverage = workPage.area / Double(workSize.width * workSize.height)
        let detection = PageDetection(quad: workPage.points, confidence: 0.9, coverage: coverage)
        XCTAssertEqual(PhotoQuadDetector.acceptedCoreQuad(detection, imageSize: workSize), workPage)
        let scrambled = PageDetection(quad: [workPage.bottomRight, workPage.topLeft, workPage.bottomLeft, workPage.topRight],
                                      confidence: 0.9, coverage: coverage)
        XCTAssertEqual(PhotoQuadDetector.acceptedCoreQuad(scrambled, imageSize: workSize), workPage)
        XCTAssertNil(PhotoQuadDetector.acceptedCoreQuad(PageDetection(quad: workPage.points, confidence: 0.3, coverage: coverage),
                                                        imageSize: workSize))
        XCTAssertNil(PhotoQuadDetector.acceptedCoreQuad(PageDetection(quad: visionFrame.points, confidence: 0.9, coverage: 0.97),
                                                        imageSize: workSize))
    }

    func testEdgeContrastMeasuresThePaperBorder() throws {
        // A bright sheet (0.9) on a dark table (0.1): pixels 100..<300 × 80..<220.
        let w = 400
        let h = 300
        var pixels = [Float](repeating: 0.1, count: w * h)
        for y in 80..<220 {
            for x in 100..<300 {
                pixels[y * w + x] = 0.9
            }
        }
        let gray = GrayImage(width: w, height: h, pixels: pixels)
        let sheetOutline = PhotoQuad(topLeft: CGPoint(x: 100, y: 80), topRight: CGPoint(x: 300, y: 80),
                                     bottomRight: CGPoint(x: 300, y: 220), bottomLeft: CGPoint(x: 100, y: 220))
        XCTAssertEqual(try XCTUnwrap(PhotoQuadDetector.edgeContrast(sheetOutline, in: gray)), 0.8, accuracy: 1e-3)
        // An outline inside the sheet (or anywhere on a featureless image) has no border.
        let inside = PhotoQuad(topLeft: CGPoint(x: 130, y: 110), topRight: CGPoint(x: 270, y: 110),
                               bottomRight: CGPoint(x: 270, y: 190), bottomLeft: CGPoint(x: 130, y: 190))
        XCTAssertEqual(try XCTUnwrap(PhotoQuadDetector.edgeContrast(inside, in: gray)), 0, accuracy: 1e-6)
        let blank = GrayImage(width: w, height: h, pixels: [Float](repeating: 0.5, count: w * h))
        XCTAssertEqual(try XCTUnwrap(PhotoQuadDetector.edgeContrast(sheetOutline, in: blank)), 0, accuracy: 1e-6)
        // An outline on the image border cannot be measured.
        XCTAssertNil(PhotoQuadDetector.edgeContrast(PhotoQuad.full(CGSize(width: w, height: h)), in: gray))
    }

    func testCorePageFinderFindsTheSheet() throws {
        let page = try markerPage()
        var camera = SyntheticPhoto.Camera()
        camera.width = 1200
        camera.height = 1600
        camera.fill = 0.72
        let photo = try SyntheticPhoto.make(page: page, camera: camera)
        let size = CGSize(width: 1200, height: 1600)
        let raster = try XCTUnwrap(RGBARaster(photo.image))
        XCTAssertEqual(raster.bytesPerRow, 4800)
        XCTAssertEqual(raster.bytes.count, 4800 * 1600)

        // The core alone: close enough for the edge snapping to take over, in TL, TR, BR, BL order.
        let found = try XCTUnwrap(PhotoQuadDetector.corePage(in: raster), "the core found no page")
        XCTAssertGreaterThanOrEqual(found.confidence, PhotoQuadDetector.minimumCoreConfidence)
        XCTAssertEqual(found.coverage, photo.corners.area / Double(size.width * size.height), accuracy: 0.05)
        let raw = try XCTUnwrap(PhotoQuad(points: found.quad))
        for (a, b) in zip(raw.points, photo.corners.points) {
            assertClose(a, b, PhotoQuadDetector.searchFraction * 1600, "core corner")
        }

        // The whole detector (Vision too, whatever it says here): the sheet, snapped to its edges,
        // and the corner editor opens.
        let analysis = PhotoQuadDetector.analyzeSync(photo.image)
        let chosen = try XCTUnwrap(analysis.quad, analysis.summary)
        for (a, b) in zip(chosen.points, photo.corners.points) {
            assertClose(a, b, 0.01 * 1600, analysis.summary)
        }
        XCTAssertEqual(PhotoGeometry.triage(chosen, imageSize: size), .needsCorners(chosen))

        // A featureless image has no page.
        let ctx = try XCTUnwrap(TileStore.makeContext(width: 1200, height: 700))
        ctx.setFillColor(CGColor(red: 0.9, green: 0.85, blue: 0.7, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 700))
        let blank = try XCTUnwrap(RGBARaster(try XCTUnwrap(ctx.makeImage())))
        XCTAssertNil(PhotoQuadDetector.corePage(in: blank))
    }

    func testLineFitHelpers() throws {
        var points: [CGPoint] = []
        for i in 0..<20 {
            let t = Double(i)
            points.append(CGPoint(x: t * 10, y: 5 + t * 5))
        }
        points.append(CGPoint(x: 50, y: 200))
        let line = try XCTUnwrap(PhotoQuadDetector.robustLine(points, minimumInliers: 10))
        XCTAssertLessThan(PhotoQuadDetector.distanceToLine(CGPoint(x: 100, y: 55), line), 0.01)
        let vertical = PhotoQuadDetector.Line(point: CGPoint(x: 7, y: 0), direction: CGVector(dx: 0, dy: 1))
        let p = try XCTUnwrap(PhotoQuadDetector.intersection(line, vertical))
        XCTAssertEqual(p.x, 7, accuracy: 1e-6)
        XCTAssertEqual(p.y, 8.5, accuracy: 1e-6)
        XCTAssertNil(PhotoQuadDetector.intersection(vertical, vertical))
    }

    // MARK: - Corner editor geometry

    func testCornerEditorViewMapping() {
        let image = CGSize(width: 3000, height: 4000)
        let fit = CornerEditorView.fitRect(image: image, in: CGRect(x: 0, y: 0, width: 390, height: 600))
        XCTAssertEqual(fit.minX, 0, accuracy: 1e-9)
        XCTAssertEqual(fit.minY, 40, accuracy: 1e-9)
        XCTAssertEqual(fit.width, 390, accuracy: 1e-9)
        XCTAssertEqual(fit.height, 520, accuracy: 1e-9)
        let v = CornerEditorView.viewPoint(CGPoint(x: 1500, y: 1000), imageSize: image, fit: fit)
        XCTAssertEqual(v.x, 195, accuracy: 1e-9)
        XCTAssertEqual(v.y, 170, accuracy: 1e-9)
        let back = CornerEditorView.imagePoint(v, imageSize: image, fit: fit)
        XCTAssertEqual(back.x, 1500, accuracy: 1e-6)
        XCTAssertEqual(back.y, 1000, accuracy: 1e-6)
        XCTAssertEqual(CornerEditorView.imagePoint(CGPoint(x: -50, y: 10_000), imageSize: image, fit: fit),
                       CGPoint(x: 0, y: 4000))
        let vector = QuadVector(sheet.points)
        XCTAssertEqual(vector.points, sheet.points)
        var half = vector
        half.scale(by: 0.5)
        XCTAssertEqual((half + half).points, sheet.points)
        XCTAssertEqual((vector - vector).magnitudeSquared, 0)
    }

    // MARK: - Georeference carried over when corners change

    func testGeoreferenceCarriesOverWhenCornersChange() throws {
        // Ground truth for the photo itself: photo pixel → Mercator.
        let photoToMercator = Homography([2.0, 0.1, 3_000_000, -0.05, -2.0, -2_900_000, 0, 0, 1])
        let old = sheet
        let new = PhotoQuad(topLeft: CGPoint(x: 400, y: 400), topRight: CGPoint(x: 2620, y: 500),
                            bottomRight: CGPoint(x: 2480, y: 3520), bottomLeft: CGPoint(x: 400, y: 3280))
        let oldSize = PhotoGeometry.outputSize(for: old, imageSize: photoSize)
        let newSize = PhotoGeometry.outputSize(for: new, imageSize: photoSize)
        let oldRect = try XCTUnwrap(PhotoGeometry.rectification(quad: old, width: oldSize.width, height: oldSize.height))
        let newRect = try XCTUnwrap(PhotoGeometry.rectification(quad: new, width: newSize.width, height: newSize.height))
        let oldGeoref = Georeference(pixelToMercator: PhotoGeometry.compose(photoToMercator, try XCTUnwrap(oldRect.inverse)),
                                     method: .grid, confidence: 0.9, rmsMeters: 3)
        let carried = try XCTUnwrap(MapImporter.carriedGeoreference(oldGeoref, oldCorners: old, oldWidth: oldSize.width,
                                                                    oldHeight: oldSize.height, newCorners: new,
                                                                    newWidth: newSize.width, newHeight: newSize.height))
        XCTAssertEqual(carried.method, .grid)
        for p in [CGPoint(x: 800, y: 900), CGPoint(x: 2000, y: 3000), CGPoint(x: 1500, y: 2000)] {
            let a = carried.pixelToMercator.apply(simd(PhotoGeometry.apply(newRect, p)))
            let b = photoToMercator.apply(simd(p))
            XCTAssertEqual(a.x, b.x, accuracy: 0.05)
            XCTAssertEqual(a.y, b.y, accuracy: 0.05)
        }
        XCTAssertNil(MapImporter.carriedGeoreference(nil, oldCorners: old, oldWidth: 10, oldHeight: 10,
                                                     newCorners: new, newWidth: 10, newHeight: 10))
    }

    private func simd(_ p: CGPoint) -> SIMD2<Double> { SIMD2(Double(p.x), Double(p.y)) }

    // MARK: - Model

    func testMapDocumentPhotoFieldsAreBackwardCompatible() throws {
        var doc = MapDocument(id: UUID(), name: "Paper map", createdAt: Date(timeIntervalSince1970: 1_700_000_000),
                              lastOpenedAt: nil, sourceFileName: "source.pdf", sourceKind: .pdf, pixelWidth: 100,
                              pixelHeight: 80, renderDPI: 300, tileSize: 512, levels: 1, georef: nil, georefMessage: nil,
                              pois: [], contourRGB: nil)
        XCTAssertNil(doc.photoSource)
        XCTAssertFalse(doc.isPhoto)
        XCTAssertFalse(doc.canAdjustCorners)
        // Written without photo fields (as before photo support) → still decodes.
        let plain = try MapLibrary.makeEncoder().encode(doc)
        let plainJSON = try XCTUnwrap(JSONSerialization.jsonObject(with: plain) as? [String: Any])
        XCTAssertNil(plainJSON["photoSource"])
        XCTAssertEqual(try MapLibrary.makeDecoder().decode(MapDocument.self, from: plain), doc)
        XCTAssertEqual(plainJSON["sourceKind"] as? String, "pdf")

        doc.sourceKind = .photo
        doc.sourceFileName = "source.jpg"
        doc.photoSource = PhotoSource(fileName: "source-photo.heic", pixelWidth: 3000, pixelHeight: 4000,
                                      quad: sheet.json, focalLength35mm: 26)
        XCTAssertTrue(doc.isPhoto)
        XCTAssertTrue(doc.canAdjustCorners)
        let data = try MapLibrary.makeEncoder().encode(doc)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(json["sourceKind"] as? String, "photo")
        let decoded = try MapLibrary.makeDecoder().decode(MapDocument.self, from: data)
        XCTAssertEqual(decoded, doc)
        XCTAssertEqual(PhotoQuad(json: try XCTUnwrap(decoded.photoSource?.quad)), sheet)
    }

    func testGeorefRequestCarriesThePhotoFlag() throws {
        var request = GeorefRequest()
        let off = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(off["photo"] as? Bool, false)
        request.photo = true
        let on = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(on["photo"] as? Bool, true)
    }

    func testCaptureInfoFromMetadata() {
        let camera = PhotoCaptureInfo.from(properties: [
            kCGImagePropertyExifDictionary: [kCGImagePropertyExifExposureTime: 0.01, kCGImagePropertyExifFocalLenIn35mmFilm: 26] as [CFString: Any],
        ])
        XCTAssertTrue(camera.isCamera)
        XCTAssertEqual(camera.focalLength35mm, 26)
        let screenshot = PhotoCaptureInfo.from(properties: [
            kCGImagePropertyExifDictionary: [kCGImagePropertyExifUserComment: "Screenshot"] as [CFString: Any],
        ])
        XCTAssertFalse(screenshot.isCamera)
        XCTAssertNil(screenshot.focalLength35mm)
        XCTAssertFalse(PhotoCaptureInfo.from(properties: [:]).isCamera)
        XCTAssertEqual(MapImporter.photoFileName(pathExtension: "HEIC"), "source-photo.heic")
        XCTAssertEqual(MapImporter.photoFileName(pathExtension: ""), "source-photo")
    }
}
