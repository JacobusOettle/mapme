import CoreGraphics
import CoreLocation
import XCTest
@testable import MapMe

/// The real thing on the Grootkloof sheet: PDF render → Vision OCR → grid georeference, and
/// the contour-vs-terrain path with the fixture DEM tiles.
final class GeoreferenceE2ETests: XCTestCase {
    /// Render pixels per map.jpg pixel (the PDF's scan maps 1:1 at 300 dpi).
    private func renderScale(_ image: CGImage, dpi: Double?) throws -> Double {
        let truth = try GrootkloofTruth.load()
        if let dpi { return dpi / 300 }
        return Double(image.width) / truth.imageWidth
    }

    private static func matches(_ text: String, _ pattern: String) -> String? {
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let m = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
              m.numberOfRanges > 1, let r = Range(m.range(at: 1), in: text)
        else { return nil }
        return String(text[r])
    }

    /// Keep raw data in the result bundle (CI exports it next to the screenshots).
    private func attachJSON<T: Encodable>(_ value: T, name: String) {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(value) else { return }
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.json")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testOCRReadsGridLabelsAndGridGeoreferenceIsAccurate() async throws {
        let (image, dpi) = try Fixtures.renderedPDF()
        XCTAssertEqual(Double(image.width), 3508, accuracy: 2)
        XCTAssertEqual(Double(image.height), 4962, accuracy: 2)
        let k = try renderScale(image, dpi: dpi)

        let started = Date()
        let ocr = try await OCRService.recognize(in: image)
        let ocrSeconds = Date().timeIntervalSince(started)
        XCTAssertFalse(ocr.isEmpty, "Vision found no text")
        attachJSON(ocr, name: "grootkloof-vision-ocr.json")

        // 25°49'00"S … 25°53'00"S on the left edge (vertical), 27°23'00"E … 27°25'00"E along the bottom.
        let latMinutes = Set(ocr.compactMap { Self.matches($0.text, #"25\D{0,3}(49|50|51|52|53)(?:\D|00)"#) })
        let lonMinutes = Set(ocr.compactMap { Self.matches($0.text, #"27\D{0,3}(23|24|25)(?:\D|00)"#) })
        let gridTexts = ocr.filter { $0.text.contains("25") || $0.text.contains("27") }.map { "\($0.text) \($0.bbox)" }
        XCTAssertGreaterThanOrEqual(latMinutes.count, 4, "latitude labels: \(latMinutes.sorted()) from \(gridTexts)")
        XCTAssertGreaterThanOrEqual(lonMinutes.count, 2, "longitude labels: \(lonMinutes.sorted()) from \(gridTexts)")
        // Vertical labels come back as tall boxes on the left margin.
        let vertical = ocr.filter { Self.matches($0.text, #"25\D{0,3}(49|50|51|52|53)(?:\D|00)"#) != nil && $0.bbox[3] > $0.bbox[2] }
        XCTAssertTrue(vertical.allSatisfy { $0.bbox[0] < 0.2 * Double(image.width) }, "\(vertical)")

        let session = try GeoreferenceService.makeSession(from: image)
        var request = GeorefRequest()
        request.ocr = ocr
        request.dpi = dpi
        request.methods = ["grid"]
        let response = try Fixtures.georeference(session, request)
        attachJSON(response, name: "grootkloof-grid-response.json")
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        XCTAssertEqual(response.method, "grid")
        let g = try XCTUnwrap(response.georeference)
        let truth = try GrootkloofTruth.load()
        let worst = truth.maxGroundError(of: g.pixelToMercator, scale: k)
        XCTAssertLessThan(worst, 25, "grid georeference max error \(worst) m")
        print("E2E grid: OCR \(ocr.count) boxes in \(String(format: "%.1f", ocrSeconds)) s, max error \(String(format: "%.1f", worst)) m")
    }

    func testGridGeoreferenceWithFixtureOCR() throws {
        let (image, dpi) = try Fixtures.renderedPDF()
        let k = try renderScale(image, dpi: dpi)
        let ocr = try Fixtures.ocrFixture().map { o -> OCRObservation in
            var scaled = o
            scaled.bbox = o.bbox.map { $0 * k }
            return scaled
        }
        let session = try GeoreferenceService.makeSession(from: image)
        var request = GeorefRequest()
        request.ocr = ocr
        request.dpi = dpi
        request.methods = ["grid"]
        let response = try Fixtures.georeference(session, request)
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        XCTAssertEqual(response.method, "grid")
        let g = try XCTUnwrap(response.georeference)
        let worst = try GrootkloofTruth.load().maxGroundError(of: g.pixelToMercator, scale: k)
        XCTAssertLessThan(worst, 25, "max error \(worst) m")
        // Labels read off the map come back as POIs inside the sheet.
        let outcome = GeoreferenceService.outcome(from: response, hadGPS: false, searchRadiusKm: 20)
        XCTAssertNotNil(outcome.georef)
        for poi in outcome.pois {
            XCTAssertLessThan(Geodesy.distance(poi.position, Fixtures.center), 8_000, poi.name)
        }
    }

    func testContourGeoreferenceWithoutOCR() throws {
        let (image, dpi) = try Fixtures.renderedPDF()
        let k = try renderScale(image, dpi: dpi)
        // GPS 12 km north-east of the sheet's centre (8.5 km N, 8.5 km E), as in the core tests.
        let gps = LatLon(lat: Fixtures.center.lat + 0.0764, lon: Fixtures.center.lon + 0.0846)
        XCTAssertEqual(Geodesy.distance(gps, Fixtures.center), 12_000, accuracy: 300)

        let session = try GeoreferenceService.makeSession(from: image)
        var request = GeorefRequest()
        request.gps = GPSFix(lat: gps.lat, lon: gps.lon, accuracyM: 10)
        request.ocr = []
        request.dpi = dpi
        request.searchRadiusM = 20_000
        request.methods = ["grid", "contours"]
        let started = Date()
        let response = try Fixtures.georeference(session, request)
        let seconds = Date().timeIntervalSince(started)
        XCTAssertEqual(response.status, "ok", response.message ?? "")
        XCTAssertEqual(response.method, "contours")
        let g = try XCTUnwrap(response.georeference)
        let worst = try GrootkloofTruth.load().maxGroundError(of: g.pixelToMercator, scale: k)
        XCTAssertLessThan(worst, 60, "contour georeference max error \(worst) m")
        print("E2E contours: \(String(format: "%.1f", seconds)) s, max error \(String(format: "%.1f", worst)) m")
    }

    func testServiceLocatesWithOCRAndNoGPS() async throws {
        // Full GeoreferenceService path as the app runs it (Vision + core, no network needed for the grid).
        let (image, dpi) = try Fixtures.renderedPDF()
        let k = try renderScale(image, dpi: dpi)
        let service = GeoreferenceService(dem: DEMService(cacheDirectory: try makeTemporaryDirectory("dem-cache")))
        let stages = StageLog()
        let outcome = await service.locate(image: image, dpi: dpi, gps: nil, contourRGB: nil, searchRadiusKm: 20) { stage, fraction in
            stages.add(stage, fraction)
        }
        let g = try XCTUnwrap(outcome.georef, outcome.message)
        XCTAssertEqual(g.method, .grid)
        XCTAssertLessThan(try GrootkloofTruth.load().maxGroundError(of: g.pixelToMercator, scale: k), 25)
        XCTAssertTrue(stages.sawReadingText)
        XCTAssertTrue(stages.isMonotonic)
        XCTAssertEqual(stages.lastFraction, 1, accuracy: 1e-9)
    }
}

private final class StageLog {
    private let lock = NSLock()
    private var entries: [(GeorefStage, Double)] = []

    func add(_ stage: GeorefStage, _ fraction: Double) {
        lock.withLock { entries.append((stage, fraction)) }
    }

    var sawReadingText: Bool { lock.withLock { entries.contains { $0.0 == .readingText } } }
    var lastFraction: Double { lock.withLock { entries.last?.1 ?? 0 } }
    var isMonotonic: Bool {
        lock.withLock {
            zip(entries, entries.dropFirst()).allSatisfy { $0.1 <= $1.1 }
        }
    }
}
