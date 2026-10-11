import CoreGraphics
import CoreLocation
import XCTest
@testable import MapMe

/// Photos of paper maps end to end on the Grootkloof sheet → page detection (Vision and the core's
/// page finder, edge snapping) → flattening → OCR → photo-mode georeference.
///
/// The primary case is the user's real phone photo of the printed sheet (fixtures/grootkloof/photo:
/// wooden table, perspective, faint contours, and the paper folded in four, so no single homography
/// fits it). It runs as the app runs it: the importer's decoding, `PhotoQuadDetector`, the
/// importer's flattening, then `GeoreferenceService` (Vision OCR, photo mode, GPS prior inside the
/// sheet, fixture DEM tiles, no network). Errors are measured at the truth's control points inside
/// the map frame (photo pixel → flattened pixel → the full model, homography and warp → ground
/// metres). The bounds are generous: the simulator's Vision may read fewer labels than the fixture
/// OCR the core's own tests use, and those hold the tight bounds.
///
/// The synthetic phone photo of the sheet (perspective, table, uneven warm light, blur, JPEG) is
/// kept as a second case with a known geometry: its errors are measured against the sheet's
/// surveyed graticule (page → photo → flattened pixels).
final class PhotoE2ETests: XCTestCase {
    // MARK: - The real photo

    /// GPS fix inside the sheet (as in the UI tests).
    private static let realPhotoGPS = CLLocationCoordinate2D(latitude: -25.8405, longitude: 27.3920)

    private struct RealScene {
        var truth: RealPhotoTruth
        /// The photo decoded the way the importer decodes it.
        var photo: CGImage
        var analysis: PageAnalysis
        /// Corners the page was flattened with: the detected ones (the true ones if nothing was
        /// found, which `testRealPhotoSheetIsDetected` reports as a failure).
        var corners: PhotoQuad
        var flat: CGImage
        /// Photo pixels → flattened pixels (the rectification the flattening applied).
        var photoToFlat: Homography
    }

    private struct DetectionReport: Codable {
        var summary: String
        var source: String?
        var detected: [[Double]]?
        var vision: [[Double]]?
        var core: [[Double]]?
        var truth: [[Double]]
        var cornerOffsetsPx: [Double]
        var tolerancePx: Double
    }

    private struct RealPhotoReport: Codable {
        var variant: String
        var status: String
        var method: String?
        var message: String
        var confidence: Double?
        var rmsM: Double?
        var warp: Bool
        /// [cols, rows] of the warp's lattice.
        var warpLattice: [Int]?
        var warpMaxOffsetM: Double?
        /// The full model (homography + warp) at the control points.
        var error: ErrorSummary?
        /// The homography alone (warp ignored), for comparison.
        var flatModelError: ErrorSummary?
        /// The best single homography over the control points (truth.json).
        var homographyFloor: RealPhotoTruth.ErrorStats?
        var ocrCount: Int
        var ocrTexts: [String]
        var seconds: Double
        var corners: [[Double]]
        var flattenedSize: [Int]
    }

    private static var cachedRealScene: RealScene?

    /// The real photo through the importer's steps: decoding, page detection, flattening.
    private func realScene() async throws -> RealScene {
        if let cached = Self.cachedRealScene { return cached }
        let started = Date()
        let truth = try RealPhotoTruth.load()
        let truePage = try XCTUnwrap(truth.pageQuad, "page_quad in photo/truth.json")
        let url = try Fixtures.photoURL(truth.image)
        let photo = try MapImporter.decodeImage(at: url)
        XCTAssertEqual(photo.width, truth.imageWidth)
        XCTAssertEqual(photo.height, truth.imageHeight)
        let analysis = await PhotoQuadDetector.analyze(photo)
        note("Real photo page detection: \(analysis.summary)")
        let corners = analysis.quad ?? truePage
        let info = PhotoCaptureInfo.read(from: url)
        let flat = try XCTUnwrap(try MapImporter.flattened(photo, quad: corners, focalLength35mm: info.focalLength35mm),
                                 "the page outline is the whole photo: nothing to flatten")
        let rect = try XCTUnwrap(PhotoGeometry.rectification(quad: corners, width: flat.width, height: flat.height))
        let scene = RealScene(truth: truth, photo: photo, analysis: analysis, corners: corners, flat: flat, photoToFlat: rect)
        attachImage(flat, name: "real-photo-flattened")
        let seconds = Date().timeIntervalSince(started)
        note("Real photo decoded, detected and flattened (\(flat.width)×\(flat.height)) in \(String(format: "%.1f", seconds)) s")
        Self.cachedRealScene = scene
        return scene
    }

    func testRealPhotoSheetIsDetected() async throws {
        let scene = try await realScene()
        let truePage = try XCTUnwrap(scene.truth.pageQuad)
        let diagonal = (PhotoGeometry.distance(truePage.topLeft, truePage.bottomRight)
            + PhotoGeometry.distance(truePage.topRight, truePage.bottomLeft)) / 2
        let tolerance = 0.02 * diagonal
        let analysis = scene.analysis
        var offsets: [Double] = []
        if let detected = analysis.quad {
            offsets = zip(detected.points, truePage.points).map { PhotoGeometry.distance($0, $1) }
        }
        let report = DetectionReport(summary: analysis.summary, source: analysis.source?.rawValue,
                                     detected: analysis.quad?.json, vision: analysis.vision?.json, core: analysis.core?.json,
                                     truth: truePage.json, cornerOffsetsPx: offsets, tolerancePx: tolerance)
        attachJSON(report, name: "real-photo-page-detection.json")
        var outlines: [(PhotoQuad, CGColor)] = [(truePage, CGColor(red: 0.1, green: 0.85, blue: 0.2, alpha: 1))]
        if let detected = analysis.quad {
            outlines.append((detected, CGColor(red: 1, green: 0.15, blue: 0.1, alpha: 1)))
        }
        attachOutlines(scene.photo, outlines, name: "real-photo-page-detection")

        guard let detected = analysis.quad else {
            XCTFail("No sheet found in the real photo (\(analysis.summary))")
            return
        }
        let worst = offsets.max() ?? .infinity
        let offsetText = offsets.map { String(format: "%.1f", $0) }.joined(separator: ", ")
        XCTAssertLessThan(worst, tolerance,
                          "detected \(detected.json) vs true \(truePage.json): corner offsets [\(offsetText)] px (\(analysis.summary))")
        let photoSize = CGSize(width: scene.photo.width, height: scene.photo.height)
        XCTAssertEqual(PhotoGeometry.triage(detected, imageSize: photoSize), .needsCorners(detected),
                       "the corner editor must open for this photo (\(analysis.summary))")
        note("Real photo: \(analysis.source?.rawValue ?? "?") found the sheet; corner offsets [\(offsetText)] px, tolerance \(String(format: "%.1f", tolerance)) px")
    }

    func testRealPhotoLocatesWithVisionOCR() async throws {
        let scene = try await realScene()
        let (outcome, report) = try await locateRealPhoto(scene, variant: "ocr", knownText: nil)
        XCTAssertEqual(report.status, "ok", outcome.message)
        let error = try XCTUnwrap(report.error, "not located: \(outcome.message)")
        XCTAssertLessThanOrEqual(error.median, 80,
                                 "real photo (Vision OCR) \(error.text) by \(report.method ?? "-"): \(outcome.message)")
    }

    /// The fixture OCR (every label, as Vision reads the scan) instead of the simulator's Vision:
    /// tells "Vision read too little" apart from a problem in the pipeline.
    func testRealPhotoLocatesWithFixtureOCR() async throws {
        let scene = try await realScene()
        let ocr = try Fixtures.photoOCRFixture().map { $0.observation(through: scene.photoToFlat) }
        XCTAssertFalse(ocr.isEmpty)
        let (outcome, report) = try await locateRealPhoto(scene, variant: "fixture-ocr", knownText: ocr)
        XCTAssertEqual(report.status, "ok", outcome.message)
        let error = try XCTUnwrap(report.error, "not located: \(outcome.message)")
        XCTAssertLessThanOrEqual(error.median, 80,
                                 "real photo (fixture OCR) \(error.text) by \(report.method ?? "-"): \(outcome.message)")
    }

    func testRealPhotoLocatesFromContoursOnly() async throws {
        let scene = try await realScene()
        let (outcome, report) = try await locateRealPhoto(scene, variant: "contours", knownText: [])
        XCTAssertEqual(report.status, "ok", outcome.message)
        XCTAssertEqual(report.method, "contours", outcome.message)
        let error = try XCTUnwrap(report.error, "not located: \(outcome.message)")
        XCTAssertLessThanOrEqual(error.median, 90,
                                 "real photo (contours only) \(error.text) by \(report.method ?? "-"): \(outcome.message)")
    }

    /// `GeoreferenceService` on the flattened real photo as the app runs it (photo mode, GPS fix
    /// inside the sheet), with the fixture DEM tiles and no network. `knownText` nil reads the
    /// text with Vision, as the app does; `[]` gives it none (contours only). The summary is
    /// attached to the results.
    private func locateRealPhoto(_ scene: RealScene, variant: String,
                                 knownText: [OCRObservation]?) async throws -> (GeorefOutcome, RealPhotoReport) {
        let service = GeoreferenceService(dem: try Fixtures.offlineDEMService())
        let gps = CLLocation(coordinate: Self.realPhotoGPS, altitude: 1500, horizontalAccuracy: 10, verticalAccuracy: 10,
                             timestamp: Date())
        let started = Date()
        let outcome = await service.locate(image: scene.flat, dpi: nil, gps: gps, contourRGB: nil, searchRadiusKm: 20,
                                           photo: true, knownText: knownText) { _, _ in }
        let seconds = Date().timeIntervalSince(started)
        let warp = outcome.georef?.warp
        var report = RealPhotoReport(variant: variant, status: outcome.response?.status ?? "none",
                                     method: outcome.response?.method, message: outcome.message,
                                     confidence: outcome.response?.confidence, rmsM: outcome.response?.rmsM,
                                     warp: warp != nil, warpLattice: warp.map { [$0.cols, $0.rows] },
                                     warpMaxOffsetM: warp?.maxOffset, error: nil, flatModelError: nil,
                                     homographyFloor: scene.truth.homographyFloor, ocrCount: outcome.ocr.count,
                                     ocrTexts: outcome.ocr.map(\.text), seconds: seconds, corners: scene.corners.json,
                                     flattenedSize: [scene.flat.width, scene.flat.height])
        if let g = outcome.georef {
            report.error = ErrorSummary(groundErrors(g, scene))
            let flatModel = Georeference(pixelToMercator: g.pixelToMercator, method: g.method, confidence: g.confidence,
                                         rmsMeters: g.rmsMeters)
            report.flatModelError = ErrorSummary(groundErrors(flatModel, scene))
        }
        attachJSON(report, name: "real-photo-\(variant)-summary.json")
        if let response = outcome.response {
            attachJSON(response, name: "real-photo-\(variant)-response.json")
        }
        var line = "Real photo (\(variant)): \(report.status) \(report.method ?? "-"), OCR \(report.ocrCount) boxes, "
        line += "warp \(report.warp ? "yes" : "no"), \(String(format: "%.1f", seconds)) s"
        if let error = report.error { line += ", \(error.text)" }
        if let flatError = report.flatModelError { line += "; homography alone \(flatError.text)" }
        note(line + " — \(outcome.message)")
        return (outcome, report)
    }

    /// Ground error (m) of a georeference of the flattened photo at every truth control point inside
    /// the map frame: photo pixel → flattened pixel → the full model (homography and warp) → lat/lon.
    private func groundErrors(_ g: Georeference, _ scene: RealScene) -> [Double] {
        scene.truth.framedControlPoints.map { (cp: RealPhotoTruth.ControlPoint) -> Double in
            let f = PhotoGeometry.apply(scene.photoToFlat, CGPoint(x: cp.x, y: cp.y))
            return Geodesy.distance(g.latLon(forPixel: f), LatLon(lat: cp.lat, lon: cp.lon))
        }
    }

    // MARK: - The synthetic photo

    private struct Scene {
        var synthetic: SyntheticPhoto.Result
        /// The photo decoded the way the importer decodes it.
        var photo: CGImage
        /// Corners the page was flattened with: the detected ones (the true ones if detection failed,
        /// which is a test failure, so the georeference tests still run).
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

        // The importer's detection: Vision and the core's page finder, the better outline snapped
        // to the paper's edges. The simulator's Vision returns the whole frame for this photo; the
        // core must then find the sheet. Either way the chosen outline is what is checked.
        let truth = synthetic.corners
        var corners = truth
        let longSide = Double(max(photo.width, photo.height))
        let photoSize = CGSize(width: photo.width, height: photo.height)
        let analysis = await PhotoQuadDetector.analyze(photo)
        note("Page detection: \(analysis.summary)")
        if let detected = analysis.quad {
            let offsets = zip(detected.points, truth.points).map { PhotoGeometry.distance($0, $1) }
            let worst = offsets.max() ?? .infinity
            XCTAssertLessThan(worst, 0.01 * longSide,
                              "detected \(detected.json) vs true \(truth.json) (\(analysis.summary))")
            let source = analysis.source?.rawValue ?? "?"
            let offsetText = offsets.map { String(format: "%.1f", $0) }.joined(separator: ", ")
            note("\(source) found the sheet; corner offsets [\(offsetText)] px")
            XCTAssertEqual(PhotoGeometry.triage(detected, imageSize: photoSize), .needsCorners(detected),
                           "the corner editor must open for this photo (\(analysis.summary))")
            corners = detected
        } else {
            XCTFail("No sheet found in the synthetic photo (\(analysis.summary)); flattening with the true corners")
        }
        XCTAssertEqual(PhotoGeometry.triage(corners, imageSize: photoSize), .needsCorners(corners))

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

    /// Worst ground error (m) over the map frame corners and centre of a georeference of the
    /// flattened photo (the full model: homography and warp, if any).
    private func maxGroundError(_ g: Georeference, _ scene: Scene) throws -> Double {
        let truth = try GrootkloofTruth.load()
        var worst = 0.0
        for p in truth.checkPoints(scale: scene.k) {
            let f = PhotoGeometry.apply(scene.pageToFlat, p)
            let a = g.mercator(forPixel: SIMD2(Double(f.x), Double(f.y)))
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
        let worst = try maxGroundError(g, scene)
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
        let worst = try maxGroundError(g, scene)
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
        // Screenshots and other non-photos are imported as before, without the editor. An outline
        // a detector may report on a featureless image has no border in the pixels and is dropped.
        let ctx = try XCTUnwrap(TileStore.makeContext(width: 1200, height: 700))
        ctx.setFillColor(CGColor(red: 0.9, green: 0.85, blue: 0.7, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 700))
        let plainImage = try XCTUnwrap(ctx.makeImage())
        let plainAnalysis = await PhotoQuadDetector.analyze(plainImage)
        note("Plain image page detection: \(plainAnalysis.summary)")
        XCTAssertEqual(PhotoGeometry.triage(plainAnalysis.quad, imageSize: CGSize(width: 1200, height: 700)), .asIs,
                       plainAnalysis.summary)
        let data = try XCTUnwrap(TileStore.jpegData(plainImage))
        let never: PhotoCornerPicker = { request in
            XCTFail("no corner editor for a plain image (initial \(request.initial.json))")
            return request.initial
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
        encoder.nonConformingFloatEncodingStrategy = .convertToString(positiveInfinity: "inf", negativeInfinity: "-inf",
                                                                       nan: "nan")
        guard let data = try? encoder.encode(value) else { return }
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.json")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// The image, downscaled, with outlines drawn on it (for the result bundle).
    private func attachOutlines(_ image: CGImage, _ outlines: [(PhotoQuad, CGColor)], name: String) {
        guard let small = MapPhotoRectifier.downscaled(image, maxLongSide: 1200),
              let ctx = TileStore.makeContext(width: small.width, height: small.height)
        else { return }
        let width = CGFloat(small.width)
        let height = CGFloat(small.height)
        ctx.draw(small, in: CGRect(x: 0, y: 0, width: width, height: height))
        let sx = width / CGFloat(image.width)
        let sy = height / CGFloat(image.height)
        ctx.setLineWidth(3)
        for (quad, color) in outlines {
            // Core Graphics' origin is bottom-left.
            let points = quad.points.map { CGPoint(x: $0.x * sx, y: height - $0.y * sy) }
            ctx.setStrokeColor(color)
            ctx.addLines(between: points)
            ctx.closePath()
            ctx.strokePath()
        }
        guard let drawn = ctx.makeImage() else { return }
        attachImage(drawn, name: name)
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
