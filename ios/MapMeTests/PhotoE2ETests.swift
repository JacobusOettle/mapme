import CoreGraphics
import CoreLocation
import XCTest
@testable import MapMe

/// Photos of paper maps end to end on the Grootkloof sheet: a synthetic phone photo of the printed
/// sheet (perspective, table, uneven warm light, blur, JPEG) → page detection (Vision + edge
/// snapping) → flattening → OCR → photo-mode georeference. Errors are measured against the sheet's
/// surveyed graticule through the known photo geometry (page → photo → flattened pixels).
final class PhotoE2ETests: XCTestCase {
    private struct Scene {
        var synthetic: SyntheticPhoto.Result
        /// The photo decoded the way the importer decodes it.
        var photo: CGImage
        /// Corners the page was flattened with (Vision's, or the true ones when Vision found none).
        var corners: PhotoQuad
        var flat: CGImage
        /// Page raster pixels → flattened pixels.
        var pageToFlat: Homography
        /// Page raster pixels per map.jpg pixel (truth.json is in map.jpg pixels).
        var k: Double
    }

    private static var cachedScene: Scene?

    private func sharedScene() async throws -> Scene {
        if let cached = Self.cachedScene { return cached }
        let (page, dpi) = try Fixtures.renderedPDF()
        let k = (dpi ?? 300) / 300
        let started = Date()
        let synthetic = try SyntheticPhoto.make(page: page)
        let url = try makeTemporaryDirectory("photo-e2e").appendingPathComponent("photo.jpg")
        try synthetic.jpeg.write(to: url)
        let photo = try MapImporter.decodeImage(at: url)
        XCTAssertEqual(photo.width, synthetic.image.width)
        XCTAssertEqual(photo.height, synthetic.image.height)
        attachImage(photo, name: "photo-e2e-synthetic-photo")

        let truth = synthetic.corners
        var corners = truth
        let longSide = Double(max(photo.width, photo.height))
        if let detected = await PhotoQuadDetector.detect(in: photo) {
            let offsets = zip(detected.points, truth.points).map { PhotoGeometry.distance($0, $1) }
            let worst = offsets.max() ?? .infinity
            XCTAssertLessThan(worst, 0.03 * longSide, "detected \(detected.json) vs true \(truth.json)")
            note("Vision found the sheet; corner offsets \(offsets.map { String(format: "%.1f", $0) }) px")
            corners = detected
        } else {
            note("Vision found no sheet in this environment; flattening with the true corners")
        }
        XCTAssertEqual(PhotoGeometry.triage(corners, imageSize: CGSize(width: photo.width, height: photo.height)),
                       .needsCorners(corners))

        let info = PhotoCaptureInfo.read(from: url)
        XCTAssertTrue(info.isCamera)
        let flat = try MapPhotoRectifier.flatten(photo, quad: corners, focalLength35mm: info.focalLength35mm)
        let rect = try XCTUnwrap(PhotoGeometry.rectification(quad: corners, width: flat.width, height: flat.height))
        let scene = Scene(synthetic: synthetic, photo: photo, corners: corners, flat: flat,
                          pageToFlat: PhotoGeometry.compose(rect, synthetic.pageToPhoto), k: k)
        attachImage(flat, name: "photo-e2e-flattened")
        note("Synthetic photo, detection and flattening (\(flat.width)×\(flat.height)) in \(String(format: "%.1f", Date().timeIntervalSince(started))) s")
        Self.cachedScene = scene
        return scene
    }

    /// Worst ground error (m) over the map frame corners and centre of a georeference of the flattened photo.
    private func maxGroundError(_ h: Homography, _ scene: Scene) throws -> Double {
        let truth = try GrootkloofTruth.load()
        var worst = 0.0
        for p in truth.checkPoints(scale: scene.k) {
            let f = PhotoGeometry.apply(scene.pageToFlat, p)
            let a = h.apply(SIMD2(Double(f.x), Double(f.y)))
            let b = truth.pixelToMercator.apply(SIMD2(Double(p.x) / scene.k, Double(p.y) / scene.k))
            let lat = WebMercator.unproject(b).lat
            let dx = a.x - b.x
            let dy = a.y - b.y
            worst = max(worst, (dx * dx + dy * dy).squareRoot() * WebMercator.groundPerMercator(lat: lat))
        }
        return worst
    }

    func testPhotoGridGeoreference() async throws {
        let scene = try await sharedScene()
        let started = Date()
        let ocr = try await OCRService.recognize(in: scene.flat)
        XCTAssertFalse(ocr.isEmpty, "Vision read no text on the flattened photo")
        attachJSON(ocr, name: "photo-grid-ocr.json")
        let session = try GeoreferenceService.makeSession(from: scene.flat)
        var request = GeorefRequest()
        request.ocr = ocr
        request.photo = true
        request.methods = ["grid"]
        let response = try Fixtures.georeference(session, request)
        attachJSON(response, name: "photo-grid-response.json")
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        let g = try XCTUnwrap(response.georeference, response.message ?? "no georeference")
        let worst = try maxGroundError(g.pixelToMercator, scene)
        XCTAssertLessThan(worst, 30, "photo grid georeference max error \(worst) m (\(response.method ?? "-"))")
        note("Photo grid: \(response.method ?? "-"), OCR \(ocr.count) boxes, \(String(format: "%.1f", Date().timeIntervalSince(started))) s, max error \(String(format: "%.1f", worst)) m")
    }

    func testPhotoContourGeoreference() async throws {
        let scene = try await sharedScene()
        // GPS 12 km north-east of the sheet's centre, as in the PDF contour test.
        let gps = LatLon(lat: Fixtures.center.lat + 0.0764, lon: Fixtures.center.lon + 0.0846)
        let session = try GeoreferenceService.makeSession(from: scene.flat)
        var request = GeorefRequest()
        request.gps = GPSFix(lat: gps.lat, lon: gps.lon, accuracyM: 10)
        request.ocr = []
        request.photo = true
        request.searchRadiusM = 20_000
        request.methods = ["grid", "contours"]
        let started = Date()
        let response = try Fixtures.georeference(session, request)
        attachJSON(response, name: "photo-contour-response.json")
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        let g = try XCTUnwrap(response.georeference, response.message ?? "no georeference")
        let worst = try maxGroundError(g.pixelToMercator, scene)
        XCTAssertLessThan(worst, 90, "photo contour georeference max error \(worst) m (\(response.method ?? "-"))")
        note("Photo contours: \(response.method ?? "-"), \(String(format: "%.1f", Date().timeIntervalSince(started))) s, max error \(String(format: "%.1f", worst)) m")
    }

    func testImporterPhotoFlowKeepsThePhotoAndAdjustsCorners() async throws {
        let scene = try await sharedScene()
        let root = try makeTemporaryDirectory("photo-import")
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let importer = MapImporter(library: library)
        let requests = CornerRequestLog()
        let picker: PhotoCornerPicker = { request in
            requests.add(request)
            return request.initial
        }
        let imported = try await importer.importImageData(scene.synthetic.jpeg, suggestedName: "Paper map",
                                                          mode: .detectPhoto(picker, fallbackCorners: scene.synthetic.corners)) { _, _ in }
        let doc = imported.document
        XCTAssertEqual(requests.count, 1, "the corner editor is asked exactly once")
        XCTAssertEqual(requests.first?.isAdjusting, false)
        XCTAssertEqual(doc.sourceKind, .photo)
        XCTAssertTrue(doc.isPhoto)
        let source = try XCTUnwrap(doc.photoSource)
        XCTAssertTrue(source.fileName.hasPrefix("source-photo."), source.fileName)
        XCTAssertEqual(doc.sourceFileName, MapImporter.flattenedFileName)
        let photoSize = CGSize(width: scene.photo.width, height: scene.photo.height)
        XCTAssertEqual(source.pixelWidth, scene.photo.width)
        XCTAssertEqual(source.pixelHeight, scene.photo.height)
        XCTAssertEqual(source.focalLength35mm ?? 0, 26, accuracy: 0.5)
        let corners = try XCTUnwrap(PhotoQuad(json: source.quad))
        let expected = PhotoGeometry.outputSize(for: corners, imageSize: photoSize,
                                                focalLength35mm: source.focalLength35mm)
        XCTAssertEqual(doc.pixelWidth, expected.width)
        XCTAssertEqual(doc.pixelHeight, expected.height)
        XCTAssertEqual(imported.baseImage.width, doc.pixelWidth)
        let fm = FileManager.default
        let folder = library.directory(for: doc.id)
        XCTAssertTrue(fm.fileExists(atPath: folder.appendingPathComponent(source.fileName).path))
        XCTAssertTrue(fm.fileExists(atPath: library.sourceURL(for: doc).path))
        XCTAssertTrue(fm.fileExists(atPath: library.thumbnailURL(for: doc).path))
        let reloaded = try await importer.loadBaseImage(for: doc)
        XCTAssertEqual(reloaded.width, doc.pixelWidth)
        XCTAssertEqual(reloaded.height, doc.pixelHeight)

        // "Adjust Corners": the editor starts from the saved corners on the original photo.
        let adjust = try await importer.loadPhoto(for: doc)
        XCTAssertTrue(adjust.isAdjusting)
        XCTAssertEqual(adjust.image.width, scene.photo.width)
        for (a, b) in zip(adjust.initial.points, corners.points) {
            XCTAssertLessThan(PhotoGeometry.distance(a, b), 1e-6)
        }
        // A located map keeps its location through the change of corners.
        var located = doc
        located.georef = Georeference(pixelToMercator: Homography([2, 0.05, 3_048_000, -0.04, -2, -2_975_000, 0, 0, 1]),
                                      method: .grid, confidence: 1, rmsMeters: 2)
        let locatedDocument = located
        await MainActor.run { library.save(locatedDocument) }
        let center = corners.centroid
        let inset = corners.mapped { p in
            CGPoint(x: p.x + (center.x - p.x) * 0.03, y: p.y + (center.y - p.y) * 0.03)
        }
        let again = try await importer.reflatten(located, photo: adjust.image, corners: inset) { _, _ in }
        let moved = again.document
        XCTAssertEqual(moved.photoSource?.quad, inset.json)
        XCTAssertEqual(moved.sourceFileName, MapImporter.flattenedFileName)
        XCTAssertLessThan(moved.pixelWidth, doc.pixelWidth)
        let carried = try XCTUnwrap(moved.georef)
        let oldRect = try XCTUnwrap(PhotoGeometry.rectification(quad: corners, width: doc.pixelWidth, height: doc.pixelHeight))
        let newRect = try XCTUnwrap(PhotoGeometry.rectification(quad: inset, width: moved.pixelWidth, height: moved.pixelHeight))
        let photoPoint = CGPoint(x: photoSize.width / 2, y: photoSize.height / 2)
        let before = try XCTUnwrap(located.georef).pixelToMercator.apply(simd(PhotoGeometry.apply(oldRect, photoPoint)))
        let after = carried.pixelToMercator.apply(simd(PhotoGeometry.apply(newRect, photoPoint)))
        XCTAssertEqual(before.x, after.x, accuracy: 0.05)
        XCTAssertEqual(before.y, after.y, accuracy: 0.05)
        let tiles = library.tilesDirectory(for: moved, dark: false)
        XCTAssertTrue(fm.fileExists(atPath: TileStore.tileURL(in: tiles, level: 0, col: 0, row: 0).path))
        XCTAssertTrue(fm.fileExists(atPath: TileStore.tileURL(in: tiles, level: moved.levels - 1, col: 0, row: 0).path))
        XCTAssertFalse(fm.fileExists(atPath: folder.appendingPathComponent("tiles-staging").path))
        let stored = await MainActor.run { library.map(id: doc.id) }
        XCTAssertEqual(stored?.pixelWidth, moved.pixelWidth)

        // "Full Image": the photo itself becomes the base raster again.
        let fullCorners = PhotoQuad.full(CGSize(width: adjust.image.width, height: adjust.image.height))
        let full = try await importer.reflatten(moved, photo: adjust.image, corners: fullCorners) { _, _ in }
        XCTAssertEqual(full.document.sourceFileName, source.fileName)
        XCTAssertEqual(full.document.pixelWidth, scene.photo.width)
        XCTAssertEqual(full.document.pixelHeight, scene.photo.height)
        XCTAssertFalse(fm.fileExists(atPath: folder.appendingPathComponent(MapImporter.flattenedFileName).path))
        let base = try await importer.loadBaseImage(for: full.document)
        XCTAssertEqual(base.width, scene.photo.width)
    }

    func testCancellingTheCornerEditorLeavesNothingBehind() async throws {
        let scene = try await sharedScene()
        let root = try makeTemporaryDirectory("photo-cancel")
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let importer = MapImporter(library: library)
        let picker: PhotoCornerPicker = { _ in nil }
        do {
            _ = try await importer.importImageData(scene.synthetic.jpeg, suggestedName: "Cancelled",
                                                   mode: .detectPhoto(picker, fallbackCorners: scene.synthetic.corners)) { _, _ in }
            XCTFail("a cancelled corner editor must cancel the import")
        } catch is CancellationError {
            // expected
        }
        let folders = try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)
        XCTAssertTrue(folders.isEmpty, "\(folders)")
        // Screenshots and other non-photos are imported as before, without the editor.
        let ctx = try XCTUnwrap(TileStore.makeContext(width: 1200, height: 700))
        ctx.setFillColor(CGColor(red: 0.9, green: 0.85, blue: 0.7, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 700))
        let data = try XCTUnwrap(TileStore.jpegData(try XCTUnwrap(ctx.makeImage())))
        let never: PhotoCornerPicker = { _ in
            XCTFail("no corner editor for a plain image")
            return nil
        }
        let plain = try await importer.importImageData(data, suggestedName: "Screenshot", mode: .detectPhoto(never)) { _, _ in }
        XCTAssertEqual(plain.document.sourceKind, .image)
        XCTAssertNil(plain.document.photoSource)
    }

    // MARK: - Helpers

    private func simd(_ p: CGPoint) -> SIMD2<Double> { SIMD2(Double(p.x), Double(p.y)) }

    private func note(_ text: String) {
        print("Photo E2E: \(text)")
        let attachment = XCTAttachment(string: text)
        attachment.name = "photo-e2e-note"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func attachJSON<T: Encodable>(_ value: T, name: String) {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(value) else { return }
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.json")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Small JPEG preview in the result bundle.
    private func attachImage(_ image: CGImage, name: String) {
        guard let small = MapPhotoRectifier.downscaled(image, maxLongSide: 1200),
              let data = TileStore.jpegData(small, quality: 0.8)
        else { return }
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.jpeg")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

/// Requests the stand-in corner editor received (it may be called from any thread).
private final class CornerRequestLog {
    private let lock = NSLock()
    private var requests: [PhotoCornerRequest] = []

    func add(_ request: PhotoCornerRequest) {
        lock.withLock { requests.append(request) }
    }

    var count: Int { lock.withLock { requests.count } }
    var first: PhotoCornerRequest? { lock.withLock { requests.first } }
}
