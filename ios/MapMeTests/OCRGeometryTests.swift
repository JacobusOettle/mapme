import CoreGraphics
import XCTest
@testable import MapMe

/// Pure geometry of the OCR pipeline: Vision boxes → processed pixels → base-image pixels.
final class OCRGeometryTests: XCTestCase {
    private func assertRect(_ r: CGRect, _ e: CGRect, accuracy: CGFloat = 1e-9, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(r.minX, e.minX, accuracy: accuracy, "minX", file: file, line: line)
        XCTAssertEqual(r.minY, e.minY, accuracy: accuracy, "minY", file: file, line: line)
        XCTAssertEqual(r.width, e.width, accuracy: accuracy, "width", file: file, line: line)
        XCTAssertEqual(r.height, e.height, accuracy: accuracy, "height", file: file, line: line)
    }

    func testVisionBoxToPixelsFlipsY() {
        let r = OCRService.visionRectToPixels(CGRect(x: 0.1, y: 0.2, width: 0.3, height: 0.4),
                                              imageSize: CGSize(width: 1000, height: 500))
        assertRect(r, CGRect(x: 100, y: 200, width: 300, height: 200), accuracy: 1e-6)
        let full = OCRService.visionRectToPixels(CGRect(x: 0, y: 0, width: 1, height: 1), imageSize: CGSize(width: 64, height: 32))
        assertRect(full, CGRect(x: 0, y: 0, width: 64, height: 32))
    }

    func testMapToBaseUpright() {
        let crop = CGRect(x: 100, y: 200, width: 1600, height: 1600)
        let r = OCRService.mapToBase(CGRect(x: 10, y: 20, width: 30, height: 5), crop: crop, rotation: .upright)
        assertRect(r, CGRect(x: 110, y: 220, width: 30, height: 5))
    }

    func testMapToBaseClockwise() {
        // Crop 300×400 turned clockwise is 400×300.
        let crop = CGRect(x: 100, y: 200, width: 300, height: 400)
        let r = OCRService.mapToBase(CGRect(x: 10, y: 20, width: 50, height: 5), crop: crop, rotation: .clockwise)
        assertRect(r, CGRect(x: 120, y: 540, width: 5, height: 50))
    }

    func testMapToBaseCounterClockwise() {
        let crop = CGRect(x: 100, y: 200, width: 300, height: 400)
        let r = OCRService.mapToBase(CGRect(x: 10, y: 20, width: 50, height: 5), crop: crop, rotation: .counterClockwise)
        assertRect(r, CGRect(x: 375, y: 210, width: 5, height: 50))
    }

    /// The pixels CoreGraphics produces for a rotated crop must agree with mapToBase.
    func testRotatedRenderingMatchesMapping() throws {
        let w = 300, h = 400
        let marker = CGRect(x: 40, y: 60, width: 20, height: 10) // top-left origin
        let ctx = try XCTUnwrap(TileStore.makeContext(width: w, height: h))
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        ctx.fill(CGRect(x: marker.minX, y: CGFloat(h) - marker.maxY, width: marker.width, height: marker.height))
        let image = try XCTUnwrap(ctx.makeImage())
        let crop = CGRect(x: 0, y: 0, width: w, height: h)
        let isRed: (UInt8, UInt8, UInt8) -> Bool = { r, g, b in r > 200 && g < 80 && b < 80 }

        let upright = try XCTUnwrap(RGBAPixels(image))
        assertRect(try XCTUnwrap(upright.bounds(where: isRed)), marker)

        for rotation in [OCRRotation.clockwise, .counterClockwise] {
            let turned = try XCTUnwrap(OCRService.rotated(image, rotation))
            XCTAssertEqual(turned.width, h)
            XCTAssertEqual(turned.height, w)
            let found = try XCTUnwrap(RGBAPixels(turned)?.bounds(where: isRed), "\(rotation)")
            assertRect(OCRService.mapToBase(found, crop: crop, rotation: rotation), marker)
        }
        // Clockwise: the top-left corner of the crop goes to the top-right.
        let cw = try XCTUnwrap(RGBAPixels(try XCTUnwrap(OCRService.rotated(image, .clockwise))))
        let foundCW = try XCTUnwrap(cw.bounds(where: isRed))
        assertRect(foundCW, CGRect(x: CGFloat(h) - marker.maxY, y: marker.minX, width: marker.height, height: marker.width))
    }

    func testSpansCoverWithOverlap() {
        for length in [1, 500, 1600, 1601, 3508, 4963] {
            let spans = OCRService.spans(length: length, tile: 1600, overlap: 0.15)
            XCTAssertFalse(spans.isEmpty)
            XCTAssertEqual(spans.first?.start, 0)
            XCTAssertEqual((spans.last?.start ?? 0) + (spans.last?.length ?? 0), length)
            for s in spans { XCTAssertLessThanOrEqual(s.length, 1600) }
            for (a, b) in zip(spans, spans.dropFirst()) {
                XCTAssertGreaterThanOrEqual(a.start + a.length - b.start, 240, "overlap for length \(length)")
                XCTAssertGreaterThan(b.start, a.start)
            }
        }
    }

    func testPlanCoversSheetAndMargins() {
        let (w, h) = (3508, 4963)
        let jobs = OCRService.plan(width: w, height: h)
        let bounds = CGRect(x: 0, y: 0, width: w, height: h)
        let upright = jobs.filter { $0.rotation == .upright }
        XCTAssertEqual(upright.count, 12)
        for job in jobs {
            XCTAssertTrue(bounds.contains(job.crop), "\(job.crop)")
            XCTAssertLessThanOrEqual(max(job.crop.width, job.crop.height), 1600)
        }
        // The vertical latitude labels on the left margin (x ≈ 116…152) are covered by clockwise passes.
        let label = CGRect(x: 116, y: 2402, width: 36, height: 185)
        XCTAssertTrue(jobs.contains { $0.rotation == .clockwise && $0.crop.contains(label) })
        XCTAssertTrue(jobs.contains { $0.rotation == .counterClockwise && $0.crop.contains(label) })
    }

    func testDeduplicate() {
        let a = OCRObservation(text: "25°49'00\"S", bbox: [116, 449, 36, 185], confidence: 0.6)
        let b = OCRObservation(text: "25°49'00\"S", bbox: [117, 450, 36, 184], confidence: 0.9)
        let partial = OCRObservation(text: "25°49'0", bbox: [116, 480, 36, 150], confidence: 0.95)
        let elsewhere = OCRObservation(text: "25°49'00\"S", bbox: [3350, 449, 36, 185], confidence: 0.7)
        let other = OCRObservation(text: "KWAMI LODGE", bbox: [2855, 4488, 178, 22], confidence: 0.93)
        let out = OCRService.deduplicate([a, partial, b, elsewhere, other])
        XCTAssertEqual(out.count, 3)
        let left = out.filter { $0.bbox[0] < 200 }
        XCTAssertEqual(left.count, 1)
        XCTAssertEqual(left.first?.text, "25°49'00\"S")
        XCTAssertTrue(out.contains { $0.text == "KWAMI LODGE" })
        XCTAssertTrue(out.contains { $0.bbox[0] > 3000 })
    }

    func testReadsCutAtInteriorTileEdgesAreDropped() throws {
        // The Grootkloof PDF at 300 dpi: 3 × 4 upright tiles of 1600 px.
        let size = CGSize(width: 3508, height: 4963)
        let tiles = OCRService.tileRects(width: 3508, height: 4963)
        XCTAssertEqual(tiles.count, 12)
        let first = try XCTUnwrap(tiles.first)
        XCTAssertEqual(first, CGRect(x: 0, y: 0, width: 1600, height: 1600))
        // "BARTLETT FARM PAI" as CI read it: its box ends 2.3 px before the first tile's right edge,
        // which lies inside the image (the next tile column starts at x = 954).
        let cutBox = CGRect(x: 1351.2, y: 1476.7, width: 246.5, height: 28.3)
        XCTAssertTrue(OCRService.touchesInteriorEdge(cutBox, crop: first, imageSize: size))
        XCTAssertFalse(OCRService.touchesInteriorEdge(CGRect(x: 1351.2, y: 1476.7, width: 200, height: 28.3),
                                                      crop: first, imageSize: size))
        // Boxes on the image's own edges are not cut by the tiling.
        XCTAssertFalse(OCRService.touchesInteriorEdge(CGRect(x: 0, y: 1, width: 40, height: 20), crop: first, imageSize: size))
        let last = try XCTUnwrap(tiles.last)
        XCTAssertEqual(last.maxX, 3508)
        XCTAssertEqual(last.maxY, 4963)
        XCTAssertFalse(OCRService.touchesInteriorEdge(CGRect(x: 3466, y: 4941, width: 41, height: 21), crop: last, imageSize: size))
        // …but the last tile's left and top edges are interior.
        let leftEdge = CGRect(x: last.minX + 1, y: last.minY + 500, width: 60, height: 20)
        XCTAssertTrue(OCRService.touchesInteriorEdge(leftEdge, crop: last, imageSize: size))
        let topEdge = CGRect(x: last.minX + 500, y: last.minY + 2.5, width: 60, height: 20)
        XCTAssertTrue(OCRService.touchesInteriorEdge(topEdge, crop: last, imageSize: size))

        // A garbled read cut at a tile edge ("IOUNT CONFERENCE & ADVENTU" is neither part nor
        // prefix of the whole label) goes when the neighbour tile read the label whole.
        let whole = OCRObservation(text: "PARAMOUNT CONFERENCE & ADVENTURE", bbox: [1116.8, 960, 520, 35.3], confidence: 0.9)
        let garbled = OCRObservation(text: "IOUNT CONFERENCE & ADVENTU", bbox: [1197, 962, 400, 30], confidence: 1)
        let cut = [OCRRead(observation: whole, atTileEdge: false), OCRRead(observation: garbled, atTileEdge: true)]
        XCTAssertEqual(OCRService.deduplicate(cut).map(\.text), ["PARAMOUNT CONFERENCE & ADVENTURE"])
        // Not at a tile edge, the same two reads are different labels to the other rules.
        XCTAssertEqual(OCRService.deduplicate([whole, garbled]).count, 2)
        // A cut read nothing else covers (a label longer than the overlap) stays, and so does one
        // only a pass of another orientation covers.
        let lone = OCRObservation(text: "MAGALIESBERG SHEET", bbox: [1300, 4600, 298, 60], confidence: 0.9)
        let rotatedWhole = OCRRead(observation: whole, atTileEdge: false, rotation: .clockwise)
        let kept = OCRService.deduplicate([OCRRead(observation: lone, atTileEdge: true),
                                           OCRRead(observation: garbled, atTileEdge: true), rotatedWhole])
        XCTAssertEqual(kept.count, 3)
        XCTAssertTrue(kept.contains { $0.text == "MAGALIESBERG SHEET" })
        XCTAssertTrue(kept.contains { $0.text == "IOUNT CONFERENCE & ADVENTU" })
    }

    func testNearPrefixDuplicatesKeepTheLongerText() {
        // As CI read them on the Grootkloof sheet: one label, two reads.
        let parking = OCRObservation(text: "BARTLETT FARM PARKING", bbox: [1351.7, 1474, 323.3, 31], confidence: 0.9)
        let pai = OCRObservation(text: "BARTLETT FARM PAI", bbox: [1351.2, 1476.7, 246.5, 28.3], confidence: 1)
        let gate = OCRObservation(text: "BARTLETT/FARM GATE", bbox: [1744.7, 1518.6, 281.4, 28.4], confidence: 1)
        XCTAssertEqual(OCRService.deduplicate([pai, parking, gate]).map(\.text), ["BARTLETT FARM PARKING", "BARTLETT/FARM GATE"])
        // Case and spaces are ignored.
        let river = OCRObservation(text: "Non-Perennial River", bbox: [958.1, 4231.9, 244.2, 23.3], confidence: 0.8)
        let riverCut = OCRObservation(text: "NON-PERENI", bbox: [958.1, 4232.5, 130, 22.8], confidence: 1)
        XCTAssertEqual(OCRService.deduplicate([riverCut, river]).map(\.text), ["Non-Perennial River"])
        // Different numbers in the same place, or the same start far apart: both stay.
        let a = OCRObservation(text: "1416", bbox: [1400, 1518, 60, 21], confidence: 1)
        let b = OCRObservation(text: "1418", bbox: [1401, 1519, 60, 21], confidence: 0.9)
        XCTAssertEqual(OCRService.deduplicate([a, b]).count, 2)
        let elsewhere = OCRObservation(text: "BARTLETT FARM PAI", bbox: [400, 3000, 246, 28], confidence: 1)
        XCTAssertEqual(OCRService.deduplicate([parking, elsewhere]).count, 2)
        // Boxes overlapping less than half of the smaller one: both stay.
        let shifted = OCRObservation(text: "BARTLETT FARM PAI", bbox: [1560, 1476.7, 246.5, 28.3], confidence: 1)
        XCTAssertEqual(OCRService.deduplicate([parking, shifted]).count, 2)

        XCTAssertTrue(OCRService.isNearPrefix("bartlettfarmpai", "bartlettfarmparking"))
        XCTAssertTrue(OCRService.isNearPrefix("riete", "rietfonteinvalley"))
        XCTAssertFalse(OCRService.isNearPrefix("1416", "1418"))
        XCTAssertFalse(OCRService.isNearPrefix("", "abc"))
    }
}

