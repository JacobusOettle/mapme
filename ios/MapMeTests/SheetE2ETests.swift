import CoreGraphics
import CoreLocation
import XCTest
@testable import MapMe

/// Sheets of the MCSA Magaliesberg 1:25 000 series as a user imports them, through the app's own
/// pipeline: the importer's PDF render → Apple Vision (`OCRService`) → `GeoreferenceService` (PDF
/// mode). Castle Gorge (Sheet 07) and Mhlabatini (Sheet 02) are the sheets a user reported the app
/// "struggles with even though they are in a similar style"; Grootkloof (Sheet 03) is the control.
///
/// Per sheet, Vision reads the render once (cached per sheet for the class: a page takes ~35 s in
/// the simulator). Its observations are attached as `<sheet>-vision-ocr.json` in the fixtures'
/// ocr.json format; the boxes are render pixels, which are map.jpg pixels (the page's image is
/// placed at exactly 300 dpi from the top-left corner), so `georef_cli --ocr` and the core's tests
/// can replay what Vision read. Then the service locates the map twice, with those observations
/// as `knownText` (what `locate` would read itself) and the app's other inputs:
/// - `far-offline`: GPS in Pretoria (-25.75, 28.19), 60–90 km east of the sheets and outside the
///   search radius (the user is probably not standing on the map), and no terrain tiles at all
///   (nothing cached, every download fails): only the grid can work;
/// - `centre-dem`: GPS at the map centre, the fixture's terrain tiles, no network.
/// Each run attaches the core's response and the POIs as the Nearby list shows them (nearest
/// first), and `<sheet>-summary.json` gathers which graticule labels Vision read (and what it read
/// where each one is printed), the outcome of both runs and their ground error at the true
/// graticule intersections (truth.json).
///
/// Grootkloof is asserted as `GeoreferenceE2ETests` asserts it. The new sheets must only not crash
/// and return a response: their outcome is recorded in the summary, not asserted, for now.
final class SheetE2ETests: XCTestCase {
    private struct Sheet {
        /// Folder of fixtures/, a folder reference in the test bundle (ios/project.yml).
        var folder: String
        var pdf: String
        /// Located today: asserted like `GeoreferenceE2ETests`.
        var isControl: Bool
    }

    private static let grootkloof = Sheet(folder: "grootkloof", pdf: "grootkloof.pdf", isControl: true)
    private static let castleGorge = Sheet(folder: "castle-gorge", pdf: "castle-gorge.pdf", isControl: false)
    private static let mhlabatini = Sheet(folder: "mhlabatini", pdf: "mhlabatini.pdf", isControl: false)

    /// Where the user probably is when importing: Pretoria, 60 km from Castle Gorge's centre,
    /// 80 km from Grootkloof's, 86 km from Mhlabatini's.
    private static let farGPS = LatLon(lat: -25.75, lon: 28.19)
    /// The search radius of these runs: the app's default when they were written (it is 50 km now,
    /// `AppSettings.searchRadiusKm`), kept because the fixtures' terrain tiles cover a 20 km search
    /// around each sheet's centre; `farGPS` is outside either radius.
    private static let searchRadiusKm = 20.0

    func testGrootkloofControl() async throws {
        try await check(Self.grootkloof)
    }

    func testCastleGorge() async throws {
        try await check(Self.castleGorge)
    }

    func testMhlabatini() async throws {
        try await check(Self.mhlabatini)
    }

    // MARK: - One sheet

    private func check(_ sheet: Sheet) async throws {
        let directory = try Fixtures.directory(sheet.folder)
        let truth = try SheetTruth.load(from: directory.appendingPathComponent("truth.json"))
        let centre = Self.centre(of: truth)

        // 1. The PDF as the importer renders it.
        let renderStarted = Date()
        let (image, dpi) = try MapImporter.renderBaseImage(from: directory.appendingPathComponent(sheet.pdf))
        let renderSeconds = Date().timeIntervalSince(renderStarted)
        // Render pixels per map.jpg pixel (truth.json is in map.jpg pixels).
        let k = (dpi ?? 300) / 300

        // 2. Apple Vision, as the app runs it.
        let vision = try await visionOCR(sheet, image)
        let ocr = vision.observations
        attachJSON(VisionOCRFile(description: Self.visionDescription(sheet), sheet: sheet.folder, system: Self.system,
                                 imageWidth: image.width, imageHeight: image.height, dpi: dpi, seconds: vision.seconds,
                                 observations: ocr),
                   name: "\(sheet.folder)-vision-ocr.json")
        let labels = labelReads(ocr, truth: truth, scale: k)

        // 3. The georeference, twice.
        let far = try await locate(sheet, run: "far-offline", image: image, dpi: dpi, ocr: ocr, gps: Self.farGPS,
                                   dem: Fixtures.offlineDEMService(tilesIn: nil), terrain: "none (offline, nothing cached)",
                                   truth: truth, centre: centre, scale: k)
        let near = try await locate(sheet, run: "centre-dem", image: image, dpi: dpi, ocr: ocr, gps: centre,
                                    dem: Fixtures.offlineDEMService(tilesIn: directory.appendingPathComponent("dem")),
                                    terrain: "fixture tiles (\(sheet.folder)/dem), no network",
                                    truth: truth, centre: centre, scale: k)

        let latRead = labels.filter { $0.axis == "lat" && $0.read }
        let lonRead = labels.filter { $0.axis == "lon" && $0.read }
        let latCount = labels.filter { $0.axis == "lat" }.count
        let lonCount = labels.filter { $0.axis == "lon" }.count
        var line = "\(sheet.folder): Vision \(ocr.count) boxes in \(String(format: "%.1f", vision.seconds)) s, "
        line += "latitude labels \(latRead.count)/\(latCount) \(latRead.map(\.label)), "
        line += "longitude labels \(lonRead.count)/\(lonCount) \(lonRead.map(\.label))"
        for report in [far.report, near.report] {
            line += " | \(report.run): \(report.status) \(report.method ?? "-")"
            if let error = report.graticuleError { line += String(format: ", max %.1f m", error.max) }
            line += ", \(report.poiCount) POIs, \(String(format: "%.1f", report.seconds)) s — \(report.message)"
        }
        let summary = SheetSummary(sheet: sheet.folder, outcome: line, system: Self.system,
                                   renderSize: [image.width, image.height], dpi: dpi, renderSeconds: renderSeconds,
                                   visionBoxes: ocr.count, visionSeconds: vision.seconds,
                                   latitudeLabelsRead: latRead.count, latitudeLabels: latCount,
                                   longitudeLabelsRead: lonRead.count, longitudeLabels: lonCount,
                                   labelsRead: (latRead + lonRead).map(\.label),
                                   labelsMissed: labels.filter { !$0.read }.map(\.label),
                                   labels: labels, runs: [far.report, near.report])
        attachJSON(summary, name: "\(sheet.folder)-summary.json")
        note(line)

        // Every sheet: the pipeline runs through and the core answers (located or not).
        for (outcome, report) in [far, near] {
            XCTAssertNotNil(outcome.response, "\(sheet.folder) \(report.run): no answer from the core: \(outcome.message)")
        }
        guard sheet.isControl else { return }

        // The control, as GeoreferenceE2ETests asserts it: Vision reads the labels, the grid locates it.
        XCTAssertGreaterThanOrEqual(latRead.count, 4, "latitude labels \(latRead.map(\.label)): \(labels)")
        XCTAssertGreaterThanOrEqual(lonRead.count, 2, "longitude labels \(lonRead.map(\.label)): \(labels)")
        for (outcome, report) in [far, near] {
            XCTAssertEqual(report.status, "ok", "\(report.run): \(outcome.message)")
            XCTAssertEqual(report.method, "grid", "\(report.run): \(outcome.message)")
            let error = try XCTUnwrap(report.graticuleError, "\(report.run): not located: \(outcome.message)")
            XCTAssertLessThan(error.max, 25, "\(report.run): \(error.text)")
            XCTAssertLessThan(report.frameMaxErrorM ?? .infinity, 25, "\(report.run): frame corners")
            for poi in outcome.pois {
                XCTAssertLessThan(Geodesy.distance(poi.position, centre), 8_000, "\(report.run): \(poi.name)")
            }
        }
    }

    // MARK: - Vision

    private struct VisionRead {
        var observations: [OCRObservation]
        var seconds: Double
    }

    /// Vision's reading of each sheet's render, by folder.
    private static var visionCache: [String: VisionRead] = [:]

    private func visionOCR(_ sheet: Sheet, _ image: CGImage) async throws -> VisionRead {
        if let cached = Self.visionCache[sheet.folder] { return cached }
        let started = Date()
        let observations = try await OCRService.recognize(in: image)
        let read = VisionRead(observations: observations, seconds: Date().timeIntervalSince(started))
        Self.visionCache[sheet.folder] = read
        return read
    }

    /// `<sheet>-vision-ocr.json`: the fixtures' ocr.json format (an object with `observations`).
    private struct VisionOCRFile: Encodable {
        var description: String
        var sheet: String
        var system: String
        var imageWidth: Int
        var imageHeight: Int
        var dpi: Double?
        var seconds: Double
        var observations: [OCRObservation]

        enum CodingKeys: String, CodingKey {
            case description, sheet, system, dpi, seconds, observations
            case imageWidth = "image_width"
            case imageHeight = "image_height"
        }
    }

    private static func visionDescription(_ sheet: Sheet) -> String {
        "Apple Vision (the app's OCRService: upright 1600 px tiles overlapping 15 %, clockwise and "
            + "counter-clockwise passes over the left and right 18 % margin strips, de-duplicated) on "
            + "\(sheet.pdf) rendered by MapImporter. bbox = [x, y, w, h] in render pixels, top-left origin; "
            + "the page image sits at 300 dpi from the top-left corner, so these are map.jpg pixels."
    }

    private static var system: String {
        #if targetEnvironment(simulator)
        let kind = "simulator"
        #else
        let kind = "device"
        #endif
        return "iOS \(ProcessInfo.processInfo.operatingSystemVersionString), \(kind)"
    }

    // MARK: - Graticule labels

    /// One printed graticule label (a truth.json line) and what Vision made of it.
    private struct LabelRead: Codable {
        /// "25°47'S", "27°34'E".
        var label: String
        /// "lat" or "lon".
        var axis: String
        /// The line's position in render pixels: y for a latitude, x for a longitude.
        var linePx: Double
        /// Some observation reads as this label: its degrees, at most three non-digits, its minutes,
        /// then a non-digit, "00" or the end ("25°47'00"S", "25 47'00", "2547'00"S").
        var read: Bool
        var matches: [String]
        /// Everything Vision read where the label is printed (just inside the left frame for a
        /// latitude, along the bottom for a longitude): text [x, y, w, h] confidence.
        var nearby: [String]
    }

    private func labelReads(_ ocr: [OCRObservation], truth: SheetTruth, scale k: Double) -> [LabelRead] {
        // Latitude labels run vertically just inside the left frame line, centred on their line;
        // longitude labels sit just above the bottom frame line, centred on theirs.
        let left = truth.frame[0] * k
        let bottom = truth.frame[3] * k
        var reads: [LabelRead] = []
        for line in truth.latLines {
            let y = line.y * k
            let window = CGRect(x: left - 30 * k, y: y - 200 * k, width: 150 * k, height: 400 * k)
            reads.append(labelRead(ocr, value: line.lat, axis: "lat", linePx: y, window: window))
        }
        for line in truth.lonLines {
            let x = line.x * k
            let window = CGRect(x: x - 240 * k, y: bottom - 70 * k, width: 480 * k, height: 100 * k)
            reads.append(labelRead(ocr, value: line.lon, axis: "lon", linePx: x, window: window))
        }
        return reads
    }

    private func labelRead(_ ocr: [OCRObservation], value: Double, axis: String, linePx: Double,
                           window: CGRect) -> LabelRead {
        let (degrees, minutes) = Self.degreesMinutes(value)
        let hemisphere = axis == "lat" ? (value < 0 ? "S" : "N") : (value < 0 ? "W" : "E")
        let mm = String(format: "%02ld", minutes)
        let pattern = "(?<![0-9])\(degrees)" + #"\D{0,3}"# + mm + #"(?:\D|00|$)"#
        let regex = try? NSRegularExpression(pattern: pattern)
        let matches = ocr.filter { Self.matches($0.text, regex) }.map(\.text)
        let nearby = ocr.filter { $0.bbox.count == 4 && Self.rect($0).intersects(window) }.map { Self.describe($0) }
        return LabelRead(label: "\(degrees)°\(mm)'\(hemisphere)", axis: axis, linePx: linePx, read: !matches.isEmpty,
                         matches: matches, nearby: nearby)
    }

    /// Whole degrees and minutes of |value| (graticule lines are on whole minutes).
    private static func degreesMinutes(_ value: Double) -> (Int, Int) {
        let total = Int((abs(value) * 60).rounded())
        return (total / 60, total % 60)
    }

    private static func matches(_ text: String, _ regex: NSRegularExpression?) -> Bool {
        guard let regex else { return false }
        return regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
    }

    private static func rect(_ o: OCRObservation) -> CGRect {
        CGRect(x: o.bbox[0], y: o.bbox[1], width: o.bbox[2], height: o.bbox[3])
    }

    private static func describe(_ o: OCRObservation) -> String {
        let box = o.bbox.map { $0.isFinite ? String(Int($0.rounded())) : "\($0)" }.joined(separator: ", ")
        return "\(o.text) [\(box)] \(String(format: "%.2f", Double(o.confidence)))"
    }

    // MARK: - Georeference

    private struct RunReport: Codable {
        /// "far-offline" or "centre-dem".
        var run: String
        var gps: LatLon
        var gpsFromCentreKm: Double
        var terrain: String
        var searchRadiusKm: Double
        var status: String
        var method: String?
        /// What the app shows.
        var message: String
        /// The core's own message.
        var coreMessage: String?
        var confidence: Double?
        var rmsM: Double?
        /// Ground error (m) at the truth.json graticule intersections.
        var graticuleError: ErrorSummary?
        /// Worst ground error (m) at the map frame's corners and centre.
        var frameMaxErrorM: Double?
        var seconds: Double
        var timingsMs: [String: Double]?
        /// The labels read off the map, as the Nearby list gets them (none when not located).
        var poiCount: Int
        var poisByKind: [String: Int]
        /// Spot heights and peaks with fewer digits than most ("478" for a "1478" cut by a line).
        var shortHeights: [String]
        /// Names that are part of a longer text Vision read on the sheet ("BATI" of "MHLABATINI").
        var nameFragments: [String]
    }

    /// A POI as a row of the Nearby list shows it.
    private struct NearbyEntry: Codable {
        var name: String
        var kind: String
        var elevationM: Double?
        var distanceM: Double?
        var direction: String?
        var lat: Double
        var lon: Double
        /// Where the label is on the map (render pixels).
        var x: Double
        var y: Double
    }

    /// `GeoreferenceService.locate` as the app calls it for a PDF (no contour colour picked), with
    /// Vision's observations handed in; attaches the response and the POIs.
    private func locate(_ sheet: Sheet, run: String, image: CGImage, dpi: Double?, ocr: [OCRObservation], gps: LatLon,
                        dem: DEMService, terrain: String, truth: SheetTruth, centre: LatLon,
                        scale k: Double) async -> (outcome: GeorefOutcome, report: RunReport) {
        let service = GeoreferenceService(dem: dem)
        let fix = CLLocation(coordinate: gps.coordinate, altitude: 1500, horizontalAccuracy: 10, verticalAccuracy: 10,
                             timestamp: Date())
        let started = Date()
        let outcome = await service.locate(image: image, dpi: dpi, gps: fix, contourRGB: nil,
                                           searchRadiusKm: Self.searchRadiusKm, photo: false, knownText: ocr) { _, _ in }
        let seconds = Date().timeIntervalSince(started)
        let response = outcome.response
        var kinds: [String: Int] = [:]
        for poi in outcome.pois { kinds[poi.kind.rawValue, default: 0] += 1 }
        var report = RunReport(run: run, gps: gps, gpsFromCentreKm: Geodesy.distance(gps, centre) / 1000, terrain: terrain,
                               searchRadiusKm: Self.searchRadiusKm, status: response?.status ?? "none",
                               method: response?.method, message: outcome.message, coreMessage: response?.message,
                               confidence: response?.confidence, rmsM: response?.rmsM, graticuleError: nil,
                               frameMaxErrorM: nil, seconds: seconds, timingsMs: response?.timingsMs,
                               poiCount: outcome.pois.count, poisByKind: kinds,
                               shortHeights: Self.shortHeights(outcome.pois),
                               nameFragments: Self.nameFragments(outcome.pois, ocr: ocr))
        if let g = outcome.georef {
            report.graticuleError = ErrorSummary(Self.graticuleErrors(g, truth: truth, scale: k))
            report.frameMaxErrorM = truth.maxGroundError(of: g.pixelToMercator, scale: k)
            let rows = NearbyRow.rows(for: outcome.pois, from: gps)
            let entries = rows.map { (row: NearbyRow) -> NearbyEntry in
                let px = g.pixel(for: row.poi.position)
                return NearbyEntry(name: row.poi.name, kind: row.poi.kind.rawValue, elevationM: row.poi.elevationM,
                                   distanceM: row.distance.map { $0.rounded() },
                                   direction: row.bearing.map { Geodesy.compassPoint($0) },
                                   lat: row.poi.position.lat, lon: row.poi.position.lon,
                                   x: Double(px.x).rounded(), y: Double(px.y).rounded())
            }
            attachJSON(entries, name: "\(sheet.folder)-\(run)-pois.json")
        }
        if let response {
            attachJSON(response, name: "\(sheet.folder)-\(run)-response.json")
        }
        return (outcome, report)
    }

    /// Ground distance (m) between where `g` and the truth put each graticule intersection.
    private static func graticuleErrors(_ g: Georeference, truth: SheetTruth, scale k: Double) -> [Double] {
        var errors: [Double] = []
        for la in truth.latLines {
            for lo in truth.lonLines {
                let p = CGPoint(x: lo.x * k, y: la.y * k)
                errors.append(Geodesy.distance(g.latLon(forPixel: p), LatLon(lat: la.lat, lon: lo.lon)))
            }
        }
        return errors
    }

    /// Centre of the map frame.
    private static func centre(of truth: SheetTruth) -> LatLon {
        let f = truth.frame
        return WebMercator.unproject(truth.pixelToMercator.apply(SIMD2((f[0] + f[2]) / 2, (f[1] + f[3]) / 2)))
    }

    /// Heights with fewer digits than the longest count at least three heights share (one stray
    /// long misread does not set it).
    private static func shortHeights(_ pois: [MapPOI]) -> [String] {
        let heights = pois.compactMap { (p: MapPOI) -> (name: String, digits: Int)? in
            guard p.kind == .spotHeight || p.kind == .peak, let e = p.elevationM, e.isFinite, e >= 1 else { return nil }
            return (name: p.name, digits: String(Int(e.rounded())).count)
        }
        var shared = 0
        for digits in Set(heights.map { $0.digits }) where heights.filter({ $0.digits == digits }).count >= 3 {
            shared = max(shared, digits)
        }
        return heights.filter { $0.digits < shared }.map { $0.name }
    }

    /// "name ⊂ longer text" for POI names whose letters are part of a longer text Vision read.
    private static func nameFragments(_ pois: [MapPOI], ocr: [OCRObservation]) -> [String] {
        let texts = Set(ocr.map { letters($0.text) }).sorted {
            $0.count != $1.count ? $0.count > $1.count : $0 < $1
        }
        var fragments: [String] = []
        for poi in pois {
            let key = letters(poi.name)
            guard key.count >= 2 else { continue }
            if let whole = texts.first(where: { $0.count > key.count && $0.contains(key) }) {
                fragments.append("\(poi.name) ⊂ \(whole)")
            }
        }
        return fragments
    }

    private static func letters(_ text: String) -> String {
        String(text.uppercased().filter { $0.isLetter })
    }

    // MARK: - Summary and attachments

    private struct SheetSummary: Codable {
        var sheet: String
        /// One line: labels read, and per run status, method, worst error, POIs, time and message.
        var outcome: String
        var system: String
        var renderSize: [Int]
        var dpi: Double?
        var renderSeconds: Double
        var visionBoxes: Int
        var visionSeconds: Double
        var latitudeLabelsRead: Int
        var latitudeLabels: Int
        var longitudeLabelsRead: Int
        var longitudeLabels: Int
        var labelsRead: [String]
        var labelsMissed: [String]
        var labels: [LabelRead]
        var runs: [RunReport]
    }

    private func note(_ text: String) {
        print("Sheet E2E: \(text)")
        let attachment = XCTAttachment(string: text)
        attachment.name = "sheet-e2e-note"
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
}
