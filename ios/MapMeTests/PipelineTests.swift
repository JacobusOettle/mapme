import CoreGraphics
import CoreLocation
import XCTest
@testable import MapMe

/// Library storage, rendering, tiling and the small pure pieces of the pipeline.
final class PipelineTests: XCTestCase {
    private func sampleDocument(id: UUID = UUID(), name: String = "Test map", createdAt: Date = Date(),
                                width: Int = 1500, height: Int = 900, levels: Int = 3) -> MapDocument {
        MapDocument(id: id, name: name, createdAt: createdAt, lastOpenedAt: nil, sourceFileName: "source.pdf",
                    sourceKind: .pdf, pixelWidth: width, pixelHeight: height, renderDPI: 300, tileSize: 512,
                    levels: levels, georef: nil, georefMessage: nil, pois: [], contourRGB: nil)
    }

    // MARK: - MapLibrary

    func testLibrarySaveReloadRenameTouchDelete() throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        XCTAssertTrue(library.maps.isEmpty)

        let older = sampleDocument(name: "Older", createdAt: Date(timeIntervalSinceNow: -3600))
        var newer = sampleDocument(name: "Newer", createdAt: Date())
        let truth = try GrootkloofTruth.load()
        newer.georef = truth.georeference
        newer.pois = [MapPOI(name: "Peak", kind: .peak, source: .mapLabel, position: Fixtures.center, elevationM: 1523)]
        library.save(older)
        library.save(newer)
        XCTAssertEqual(library.maps.map(\.name), ["Newer", "Older"])
        XCTAssertTrue(FileManager.default.fileExists(atPath: library.directory(for: newer.id).appendingPathComponent("meta.json").path))

        let reloaded = MapLibrary(rootURL: root)
        XCTAssertEqual(reloaded.maps.map(\.name), ["Newer", "Older"])
        let loaded = try XCTUnwrap(reloaded.map(id: newer.id))
        XCTAssertEqual(loaded.georef?.pixelToMercator, newer.georef?.pixelToMercator)
        XCTAssertEqual(loaded.georef?.method, .grid)
        XCTAssertEqual(loaded.pois.first?.name, "Peak")
        XCTAssertEqual(loaded.pois.first?.kind, .peak)

        reloaded.touch(older.id)
        XCTAssertEqual(reloaded.maps.first?.id, older.id)
        XCTAssertNotNil(reloaded.map(id: older.id)?.lastOpenedAt)

        reloaded.rename(older, to: "  Renamed  ")
        XCTAssertEqual(reloaded.map(id: older.id)?.name, "Renamed")
        reloaded.rename(older, to: "   ")
        XCTAssertEqual(reloaded.map(id: older.id)?.name, "Renamed")

        reloaded.delete(try XCTUnwrap(reloaded.map(id: older.id)))
        XCTAssertNil(reloaded.map(id: older.id))
        XCTAssertFalse(FileManager.default.fileExists(atPath: reloaded.directory(for: older.id).path))
        XCTAssertEqual(MapLibrary(rootURL: root).maps.map(\.id), [newer.id])
    }

    // MARK: - PDF rendering

    func testPDFScaleClamps() {
        XCTAssertEqual(MapImporter.pdfScale(for: CGSize(width: 842, height: 1191)), 300.0 / 72.0, accuracy: 1e-9)
        // Tiny page: long side raised to 2048 px.
        XCTAssertEqual(MapImporter.pdfScale(for: CGSize(width: 100, height: 50)) * 100, 2048, accuracy: 1e-6)
        // Huge page: long side ≤ 6000 px and ≤ 36 MP.
        let s = MapImporter.pdfScale(for: CGSize(width: 3000, height: 2900))
        XCTAssertLessThanOrEqual(3000 * s, 6000.001)
        XCTAssertLessThanOrEqual(3000 * s * 2900 * s, 36_000_001)
    }

    func testPDFRenderPlanHonoursRotation() {
        let box = CGRect(x: 10, y: 20, width: 842, height: 1191)
        for rotation in [0, 90, 180, 270, -90, 450] {
            let plan = MapImporter.pdfRenderPlan(box: box, rotation: rotation)
            let r = ((rotation % 360) + 360) % 360
            let portrait = r == 0 || r == 180
            XCTAssertEqual(plan.dpi, 300, accuracy: 1e-9)
            XCTAssertEqual(plan.width, portrait ? 3508 : 4963)
            XCTAssertEqual(plan.height, portrait ? 4963 : 3508)
            let corners = [CGPoint(x: box.minX, y: box.minY), CGPoint(x: box.maxX, y: box.minY),
                           CGPoint(x: box.maxX, y: box.maxY), CGPoint(x: box.minX, y: box.maxY)]
                .map { $0.applying(plan.transform) }
            let shownW = (portrait ? box.width : box.height) * plan.scale
            let shownH = (portrait ? box.height : box.width) * plan.scale
            let top = CGFloat(plan.height)
            for c in corners {
                XCTAssertTrue(abs(c.x) < 1e-6 || abs(c.x - shownW) < 1e-6, "x \(c.x) rotation \(rotation)")
                XCTAssertTrue(abs(c.y - top) < 1e-6 || abs(c.y - (top - shownH)) < 1e-6, "y \(c.y) rotation \(rotation)")
            }
            // Which page corner lands at the bitmap's top-left (CoreGraphics y-up: (0, height)).
            let expectedTopLeft: CGPoint
            switch r {
            case 90: expectedTopLeft = CGPoint(x: box.minX, y: box.minY)
            case 180: expectedTopLeft = CGPoint(x: box.maxX, y: box.minY)
            case 270: expectedTopLeft = CGPoint(x: box.maxX, y: box.maxY)
            default: expectedTopLeft = CGPoint(x: box.minX, y: box.maxY)
            }
            let p = expectedTopLeft.applying(plan.transform)
            XCTAssertEqual(p.x, 0, accuracy: 1e-6, "rotation \(rotation)")
            XCTAssertEqual(p.y, top, accuracy: 1e-6, "rotation \(rotation)")
        }
    }

    func testRenderGrootkloofPDF() throws {
        let (image, dpi) = try Fixtures.renderedPDF()
        XCTAssertEqual(Double(image.width), 3508, accuracy: 2)
        XCTAssertEqual(Double(image.height), 4962, accuracy: 2)
        XCTAssertEqual(try XCTUnwrap(dpi), 300, accuracy: 0.01)
        // The page's embedded scan maps 1:1 onto the raster at 300 dpi: the map frame line at
        // x = 107 (map.jpg pixels) is dark, the paper margin left of it is light.
        let pixels = try XCTUnwrap(RGBAPixels(try XCTUnwrap(image.cropping(to: CGRect(x: 60, y: 2400, width: 100, height: 40)))))
        let margin = pixels.rgb(10, 20)
        XCTAssertGreaterThan(Int(margin.r) + Int(margin.g) + Int(margin.b), 600)
        let frameLine = (45...49).map { x -> Int in
            let p = pixels.rgb(x, 20)
            return Int(p.r) + Int(p.g) + Int(p.b)
        }
        XCTAssertLessThan(frameLine.min() ?? 765, 150, "frame line at x≈107 should be dark")
    }

    func testImporterImportsPDFIntoLibrary() async throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try Fixtures.url("grootkloof.pdf")
        let library = MapLibrary(rootURL: root)
        let importer = MapImporter(library: library)
        let steps = StepLog()
        let imported = try await importer.importFile(at: source) { fraction, step in steps.add(fraction, step) }
        let doc = imported.document
        XCTAssertEqual(doc.name, "grootkloof")
        XCTAssertEqual(doc.sourceKind, .pdf)
        XCTAssertEqual(doc.sourceFileName, "source.pdf")
        XCTAssertEqual(doc.pixelWidth, imported.baseImage.width)
        XCTAssertEqual(doc.pixelHeight, imported.baseImage.height)
        XCTAssertEqual(doc.renderDPI ?? 0, 300, accuracy: 0.01)
        XCTAssertEqual(doc.tileSize, 512)
        XCTAssertEqual(doc.levels, TileStore.levelCount(width: doc.pixelWidth, height: doc.pixelHeight, tileSize: 512))
        XCTAssertTrue(steps.contains("Rendering PDF…"))
        XCTAssertTrue(steps.contains("Building tiles…"))
        XCTAssertEqual(steps.lastFraction, 1, accuracy: 1e-9)

        let fm = FileManager.default
        XCTAssertTrue(fm.fileExists(atPath: library.sourceURL(for: doc).path))
        XCTAssertTrue(fm.fileExists(atPath: library.thumbnailURL(for: doc).path))
        let tiles = library.tilesDirectory(for: doc, dark: false)
        XCTAssertTrue(fm.fileExists(atPath: TileStore.tileURL(in: tiles, level: 0, col: 0, row: 0).path))
        XCTAssertTrue(fm.fileExists(atPath: TileStore.tileURL(in: tiles, level: doc.levels - 1, col: 0, row: 0).path))
        let thumb = try XCTUnwrap(TileStore.loadImage(at: library.thumbnailURL(for: doc)))
        XCTAssertEqual(max(thumb.width, thumb.height), 640)

        let inLibrary = await MainActor.run { library.map(id: doc.id) }
        XCTAssertEqual(inLibrary?.id, doc.id)

        let again = try await importer.loadBaseImage(for: doc)
        XCTAssertEqual(again.width, doc.pixelWidth)
        XCTAssertEqual(again.height, doc.pixelHeight)
    }

    func testImporterImportsImageData() async throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let ctx = try XCTUnwrap(TileStore.makeContext(width: 1200, height: 700))
        ctx.setFillColor(CGColor(red: 0.9, green: 0.85, blue: 0.7, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 700))
        let data = try XCTUnwrap(TileStore.jpegData(try XCTUnwrap(ctx.makeImage())))
        let library = MapLibrary(rootURL: root)
        let imported = try await MapImporter(library: library).importImageData(data, suggestedName: "Screenshot") { _, _ in }
        XCTAssertEqual(imported.document.sourceKind, .image)
        XCTAssertNil(imported.document.renderDPI)
        XCTAssertEqual(imported.document.pixelWidth, 1200)
        XCTAssertEqual(imported.document.pixelHeight, 700)
        XCTAssertEqual(imported.document.levels, 3)
        XCTAssertEqual(imported.document.name, "Screenshot")
    }

    // MARK: - Tiles

    func testTilePyramidOnSyntheticImage() throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let w = 1500, h = 900
        let ctx = try XCTUnwrap(TileStore.makeContext(width: w, height: h))
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        ctx.setStrokeColor(CGColor(red: 0.6, green: 0.45, blue: 0.3, alpha: 1))
        ctx.setLineWidth(3)
        for i in 0..<12 {
            ctx.strokeEllipse(in: CGRect(x: 100 + i * 40, y: 50 + i * 25, width: 1300 - i * 80, height: 800 - i * 50))
        }
        let image = try XCTUnwrap(ctx.makeImage())

        let library = MapLibrary(rootURL: root)
        let doc = sampleDocument(width: w, height: h, levels: TileStore.levelCount(width: w, height: h, tileSize: 512))
        XCTAssertEqual(doc.levels, 3)
        let dir = library.tilesDirectory(for: doc, dark: false)
        var fractions: [Double] = []
        let lock = NSLock()
        let levels = try TileStore.buildPyramid(from: image, into: dir, tileSize: 512) { f in
            lock.withLock { fractions.append(f) }
        }
        XCTAssertEqual(levels, 3)
        XCTAssertEqual(fractions.max() ?? 0, 1, accuracy: 1e-9)

        // Expected level sizes: 1500×900, 750×450, 375×225.
        let expected: [(Int, Int, [[(Int, Int)]])] = [
            (3, 2, [[(512, 512), (512, 512), (476, 512)], [(512, 388), (512, 388), (476, 388)]]),
            (2, 1, [[(512, 450), (238, 450)]]),
            (1, 1, [[(375, 225)]]),
        ]
        let store = TileStore(map: doc, library: library)
        for (level, spec) in expected.enumerated() {
            let size = store.levelSize(level)
            XCTAssertEqual(Int(size.width), spec.2[0].reduce(0) { $0 + $1.0 })
            for row in 0..<spec.1 {
                for col in 0..<spec.0 {
                    let tile = try XCTUnwrap(store.tile(level: level, col: col, row: row, dark: false), "\(level)/\(col)_\(row)")
                    XCTAssertEqual(tile.width, spec.2[row][col].0, "\(level)/\(col)_\(row) width")
                    XCTAssertEqual(tile.height, spec.2[row][col].1, "\(level)/\(col)_\(row) height")
                }
            }
            XCTAssertNil(store.tile(level: level, col: spec.0, row: 0, dark: false))
        }
        XCTAssertNil(store.tile(level: 3, col: 0, row: 0, dark: false))

        // Dark tile: drawn by the core on first use and cached in tiles-dark-v2; paper becomes the
        // night paper (#212124).
        let dark = try XCTUnwrap(store.tile(level: 0, col: 0, row: 0, dark: true))
        XCTAssertEqual(dark.width, 512)
        XCTAssertEqual(dark.height, 512)
        let darkFile = TileStore.tileURL(in: library.tilesDirectory(for: doc, dark: true), level: 0, col: 0, row: 0)
        XCTAssertTrue(darkFile.path.contains("/tiles-dark-v2/"), darkFile.path)
        XCTAssertTrue(FileManager.default.fileExists(atPath: darkFile.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: library.darkProfileURL(for: doc).path))
        let px = try XCTUnwrap(RGBAPixels(dark)).rgb(5, 5)
        XCTAssertEqual(Double(px.r), 33, accuracy: 4, "paper should be the night paper: \(px)")
        XCTAssertEqual(Double(px.g), 33, accuracy: 4, "paper should be the night paper: \(px)")
        XCTAssertEqual(Double(px.b), 36, accuracy: 4, "paper should be the night paper: \(px)")
        let light = try XCTUnwrap(RGBAPixels(try XCTUnwrap(store.tile(level: 0, col: 0, row: 0, dark: false)))).rgb(5, 5)
        XCTAssertGreaterThan(Int(light.r) + Int(light.g) + Int(light.b), 700)
        store.purgeMemory()
        XCTAssertNotNil(store.tile(level: 0, col: 0, row: 0, dark: true))
    }

    // MARK: - Small pure pieces

    func testDEMServiceHelpers() {
        XCTAssertEqual(DEMService.remoteURL(for: TileID(z: 11, x: 1179, y: 1175))?.absoluteString,
                       "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/11/1179/1175.png")
        XCTAssertNil(DEMService.remoteURL(for: TileID(z: 2, x: 4, y: 0)))
        XCTAssertTrue(DEMService.isPNG(Fixtures.demTile(TileID(z: 11, x: 1179, y: 1175)) ?? Data()))
        XCTAssertFalse(DEMService.isPNG(Data("<html>".utf8)))
        let service = DEMService(cacheDirectory: URL(fileURLWithPath: "/tmp/dem"))
        XCTAssertEqual(service.cacheURL(for: TileID(z: 13, x: 4718, y: 4703)).path, "/tmp/dem/13/4718/4703.png")
    }

    func testOverpassQueryAndParsing() throws {
        let bbox = POIService.BoundingBox(south: -25.8923, west: 27.3666, north: -25.8074, east: 27.4334)
        let query = POIService.query(for: bbox)
        XCTAssertTrue(query.hasPrefix("[out:json][timeout:20];("))
        XCTAssertTrue(query.hasSuffix(");out body 300;"))
        XCTAssertTrue(query.contains(#"node(-25.892300,27.366600,-25.807400,27.433400)[natural~"^(peak|saddle|spring|cave_entrance|volcano)$"];"#))
        XCTAssertTrue(query.contains("[information=guidepost];"))
        let body = try XCTUnwrap(POIService.formBody(query: "a=b&c"))
        XCTAssertEqual(String(decoding: body, as: UTF8.self), "data=a%3Db%26c")

        let json = """
        {"elements": [
          {"type": "node", "id": 1, "lat": -25.84, "lon": 27.40, "tags": {"natural": "peak", "ele": "1523"}},
          {"type": "node", "id": 2, "lat": -25.85, "lon": 27.41, "tags": {"natural": "peak", "name": "Grootkloof Kop", "ele": "1480 m"}},
          {"type": "node", "id": 3, "lat": -25.86, "lon": 27.39, "tags": {"waterway": "waterfall"}},
          {"type": "node", "id": 4, "lat": -25.86, "lon": 27.38, "tags": {"amenity": "parking"}},
          {"type": "node", "id": 5, "lat": -25.86, "lon": 27.38, "tags": {"highway": "bus_stop"}},
          {"type": "node", "id": 1, "lat": -25.84, "lon": 27.40, "tags": {"natural": "peak"}}
        ]}
        """
        let response = try JSONDecoder().decode(POIService.OverpassResponse.self, from: Data(json.utf8))
        let pois = POIService.pois(from: response)
        XCTAssertEqual(pois.map(\.name), ["Peak 1523 m", "Grootkloof Kop", "Waterfall", "Parking"])
        XCTAssertEqual(pois.map(\.kind), [.peak, .peak, .water, .parking])
        XCTAssertEqual(pois[1].elevationM, 1480)
        XCTAssertTrue(pois.allSatisfy { $0.source == .openStreetMap })
        XCTAssertEqual(POIService.parseElevation("5000 ft") ?? 0, 1524, accuracy: 0.01)
        XCTAssertNil(POIService.parseElevation("high"))
    }

    func testPOIBoundingBoxNeedsGeoreference() throws {
        var doc = sampleDocument(width: 3507, height: 4960)
        XCTAssertNil(POIService.boundingBox(of: doc))
        doc.georef = try GrootkloofTruth.load().georeference
        let bbox = try XCTUnwrap(POIService.boundingBox(of: doc))
        XCTAssertEqual(bbox.north, -25.8074, accuracy: 0.002)
        XCTAssertEqual(bbox.south, -25.8923, accuracy: 0.002)
        XCTAssertEqual(bbox.west, 27.3666, accuracy: 0.002)
        XCTAssertEqual(bbox.east, 27.4334, accuracy: 0.002)
    }

    func testOutcomeFromCoreResponse() throws {
        let truth = try GrootkloofTruth.load()
        let m = truth.pixelToMercator.m.map { String($0) }.joined(separator: ", ")
        let json = """
        {"status": "ok", "method": "grid", "pixel_to_mercator": [\(m)], "confidence": 0.97, "rms_m": 1.4,
         "pois": [{"name": "KWAMI LODGE", "kind": "place", "elevation_m": null, "x": 2944.0, "y": 4499.0},
                  {"name": "1523", "kind": "spot_height", "elevation_m": 1523, "x": 2457.0, "y": 4438.0},
                  {"name": "Somewhere", "kind": "mystery", "x": 10, "y": 10}],
         "message": "Located from 5 latitude and 3 longitude grid labels (±1 m)."}
        """
        let response = try JSONDecoder().decode(GeorefResponse.self, from: Data(json.utf8))
        let outcome = GeoreferenceService.outcome(from: response, hadGPS: false, searchRadiusKm: 20)
        let g = try XCTUnwrap(outcome.georef)
        XCTAssertEqual(g.method, .grid)
        XCTAssertEqual(outcome.message, "Located from 5 latitude and 3 longitude grid labels (±1 m).")
        XCTAssertEqual(outcome.pois.map(\.kind), [.place, .spotHeight, .other])
        XCTAssertTrue(outcome.pois.allSatisfy { $0.source == .mapLabel })
        let lodge = outcome.pois[0].position
        let expected = truth.georeference.latLon(forPixel: CGPoint(x: 2944, y: 4499))
        XCTAssertLessThan(Geodesy.distance(lodge, expected), 0.01)
        XCTAssertEqual(outcome.pois[1].elevationM, 1523)

        let failed = try JSONDecoder().decode(GeorefResponse.self, from: Data(#"{"status": "failed", "message": "No grid."}"#.utf8))
        let noGPS = GeoreferenceService.outcome(from: failed, hadGPS: false, searchRadiusKm: 20)
        XCTAssertNil(noGPS.georef)
        XCTAssertTrue(noGPS.message.contains("Turn on location"), noGPS.message)
        let withGPS = GeoreferenceService.outcome(from: failed, hadGPS: true, searchRadiusKm: 20)
        XCTAssertEqual(withGPS.message, "No grid.")
    }

    func testGPSFixRequiresValidAccuracy() {
        func location(accuracy: Double) -> CLLocation {
            CLLocation(coordinate: CLLocationCoordinate2D(latitude: -25.84, longitude: 27.40), altitude: 1400,
                       horizontalAccuracy: accuracy, verticalAccuracy: 10, timestamp: Date())
        }
        XCTAssertEqual(GeoreferenceService.gpsFix(from: location(accuracy: 8)), GPSFix(lat: -25.84, lon: 27.40, accuracyM: 8))
        XCTAssertNil(GeoreferenceService.gpsFix(from: location(accuracy: -1)))
        XCTAssertNil(GeoreferenceService.gpsFix(from: nil))
    }

    func testLibraryStatusAndProximity() throws {
        var doc = sampleDocument(width: 3507, height: 4960)
        XCTAssertEqual(MapStatus.text(for: doc), "Not located")
        XCTAssertEqual(MapProximity.of(doc, from: Fixtures.center), .unknown)
        let truth = try GrootkloofTruth.load()
        doc.georef = Georeference(pixelToMercator: truth.pixelToMercator, method: .grid, confidence: 1, rmsMeters: 1.6)
        XCTAssertEqual(MapStatus.text(for: doc), "Located · grid ±2 m")
        doc.georef?.method = .contours
        doc.georef?.rmsMeters = 24.6
        XCTAssertEqual(MapStatus.text(for: doc), "Located · contours ±25 m")
        doc.georef?.method = .manual
        XCTAssertEqual(MapStatus.text(for: doc), "Manual")

        XCTAssertEqual(MapProximity.of(doc, from: Fixtures.center), .onMap)
        // 12 km north-east of the centre: away, roughly north-east.
        let away = Geodesy.destination(Fixtures.center, bearing: 45, distance: 12_000)
        guard case .away(let meters, let bearing) = MapProximity.of(doc, from: away) else {
            return XCTFail("expected .away")
        }
        XCTAssertGreaterThan(meters, 4_000)
        XCTAssertLessThan(meters, 12_000)
        XCTAssertGreaterThan(bearing, 180)
        XCTAssertLessThan(bearing, 270)
        // The off-map card names where you are relative to the map, not the way back.
        let text: String = MapProximity.offMapText(map: doc, here: away, units: .metric) ?? ""
        XCTAssertTrue(text.hasSuffix("NE of this map"), text)
    }
}

/// Collects importer progress from any thread.
private final class StepLog {
    private let lock = NSLock()
    private var steps: [String] = []
    private var fractions: [Double] = []

    func add(_ fraction: Double, _ step: String) {
        lock.withLock {
            steps.append(step)
            fractions.append(fraction)
        }
    }

    func contains(_ step: String) -> Bool { lock.withLock { steps.contains(step) } }
    var lastFraction: Double { lock.withLock { fractions.last ?? 0 } }
}
