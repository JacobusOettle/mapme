import CoreGraphics
import CoreLocation
import ImageIO
import XCTest
@testable import MapMe

/// Maps as a user imports them, through the app's own pipeline: the importer's PDF render → Apple
/// Vision (`OCRService`) → `GeoreferenceService` (PDF mode).
///
/// **MCSA Magaliesberg 1:25 000 sheets.** Castle Gorge (Sheet 07) and Mhlabatini (Sheet 02) are
/// the sheets a user reported the app "struggles with even though they are in a similar style";
/// Grootkloof (Sheet 03) is the control.
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
///
/// **Gaia GPS exports** (fixtures/gaia-langalibalele, fixtures/gaia-giants-castle): the two PDFs a
/// user reported "don't work", the first maps that are no MCSA sheet. One ~100 ppi north-up Web
/// Mercator raster on an A4 landscape page (the 300 dpi render scales it ~3×, 1.6 % more down than
/// across), thin light-brown contours every 80 / 160 ft over hillshading, spot heights in feet, the
/// app's attribution and projection printed on the page, no grid. Per map (`testGaia…`):
/// - the render is attached as a 1:1 crop, whole only when the centre run misses (`<map>-render.png`,
///   ~9 MB lossless: `georef_cli` on it with the Vision observations replays the app exactly), and
///   compared with the fixture's
///   stand-ins for it (`<map>-render-check.json`: map.jpg, the raster upsampled smoothly, against
///   map_nearest.jpg, by pixel repetition; whether the render repeats raster pixels);
/// - Vision reads it (cached per map, as for the sheets) → `<map>-vision-ocr.json`;
/// - `centre-dem`: GPS at the map centre, the fixture's tiles, the app's default 50 km search →
///   located from the contours within 100 m at all 25 truth points (case.json);
/// - `far-magaliesberg`: GPS at (-25.83, 27.5), 432 km away where the user's other maps are, with
///   the terrain there (the MCSA fixtures' tiles) and no hints → no confident wrong answer; not
///   located, the result screen offers "Search wider";
/// - the names that first search offers to look up (`hint_queries`) include Giant's Castle or
///   Langalibalele and no attribution or projection text; the POIs of both runs have no such
///   text either, and their heights are the feet printed, in metres.
/// `testGaia…SearchWider` then runs "Search wider" as the app does after that first search: a
/// stand-in for Apple Maps finds the top name where it is printed on the map (its true position
/// through the truth points) and nothing else; the sheet's suggested distance (500 km) and the
/// wide search with that one hint must locate the map within 100 m.
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
        let vision = try await visionOCR(sheet.folder, image)
        let ocr = vision.observations
        attachJSON(VisionOCRFile(description: Self.visionDescription(pdf: sheet.pdf, pixels: Self.sheetPixels),
                                 sheet: sheet.folder, system: Self.system,
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

    /// Vision's reading of each map's render, by fixture folder.
    private static var visionCache: [String: VisionRead] = [:]

    private func visionOCR(_ folder: String, _ image: CGImage) async throws -> VisionRead {
        if let cached = Self.visionCache[folder] { return cached }
        let started = Date()
        let observations = try await OCRService.recognize(in: image)
        let read = VisionRead(observations: observations, seconds: Date().timeIntervalSince(started))
        Self.visionCache[folder] = read
        return read
    }

    /// `<map>-vision-ocr.json`: the fixtures' ocr.json format (an object with `observations`).
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

    /// What the boxes' pixels are on an MCSA sheet.
    private static let sheetPixels = "the page image sits at 300 dpi from the top-left corner, so these are map.jpg pixels."
    /// What the boxes' pixels are on a Gaia export.
    private static let gaiaPixels = "the page is rendered at 300 dpi from its top-left corner (map.jpg's pixel grid; "
        + "the map raster spans 75…3435 × 75…2415, the legend band from y 2310 covers its bottom), so these are "
        + "map.jpg pixels."

    private static func visionDescription(pdf: String, pixels: String) -> String {
        "Apple Vision (the app's OCRService: upright 1600 px tiles overlapping 15 %, clockwise and "
            + "counter-clockwise passes over the left and right 18 % margin strips, de-duplicated) on "
            + "\(pdf) rendered by MapImporter. bbox = [x, y, w, h] in render pixels, top-left origin; " + pixels
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
            attachJSON(Self.nearbyEntries(outcome.pois, georef: g, from: gps), name: "\(sheet.folder)-\(run)-pois.json")
        }
        if let response {
            attachJSON(response, name: "\(sheet.folder)-\(run)-response.json")
        }
        return (outcome, report)
    }

    /// The POIs as the Nearby list shows them from `gps` (nearest first), with their map pixels.
    private static func nearbyEntries(_ pois: [MapPOI], georef g: Georeference, from gps: LatLon) -> [NearbyEntry] {
        NearbyRow.rows(for: pois, from: gps).map { (row: NearbyRow) -> NearbyEntry in
            let px = g.pixel(for: row.poi.position)
            return NearbyEntry(name: row.poi.name, kind: row.poi.kind.rawValue, elevationM: row.poi.elevationM,
                               distanceM: row.distance.map { $0.rounded() },
                               direction: row.bearing.map { Geodesy.compassPoint($0) },
                               lat: row.poi.position.lat, lon: row.poi.position.lon,
                               x: Double(px.x).rounded(), y: Double(px.y).rounded())
        }
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

    // MARK: - Gaia GPS exports

    /// A PDF exported from Gaia GPS (fixtures/gaia-*).
    private struct GaiaMap {
        /// Folder of fixtures/, a folder reference in the test bundle (ios/project.yml).
        var folder: String
        var pdf: String
        /// Part of the render attached at 1:1 (map.jpg pixels): contour lines, summits, their names.
        var crop: CGRect
    }

    /// Langalibalele Pass / Mount Durnford, 1 mi bar: 9.1 m/px raster, 3.03 × 3.08 m/px rendered,
    /// contours every 80 ft. The crop: Mount Durnford, Potterill, Kambule on the escarpment.
    private static let langalibalele = GaiaMap(folder: "gaia-langalibalele", pdf: "gaia-langalibalele.pdf",
                                               crop: CGRect(x: 600, y: 850, width: 900, height: 600))
    /// Giant's Castle, 2 mi bar: 23.0 m/px raster, 7.65 × 7.77 m/px rendered, contours every
    /// 160 ft. The crop: Giant's Castle and Giant's Pass.
    private static let giantsCastle = GaiaMap(folder: "gaia-giants-castle", pdf: "gaia-giants-castle.pdf",
                                              crop: CGRect(x: 2100, y: 1200, width: 900, height: 600))

    /// Where the user's other maps are (the Magaliesberg, by Grootkloof): 432 km from both Gaia maps,
    /// beyond the first search's 50 km.
    private static let magaliesbergGPS = LatLon(lat: -25.83, lon: 27.5)
    /// Names the first search must offer to look up on either map (`letters` of them).
    private static let expectedNames = ["GIANTSCASTLE", "LANGALIBALELE"]
    /// The page's attribution and projection text (lower-case letters): never a place or a POI.
    private static let pageFurniture = ["gaia", "openstreet", "mercator", "epsg", "declination"]
    /// Southern Africa's highest summit (Thabana Ntlenyana) is 3 482 m: a height above this is a
    /// height in feet taken for metres.
    private static let maxHeightM = 3_500.0
    /// The escarpment summits on both maps are printed 10 092–10 925 ft (3 076–3 330 m).
    private static let summitHeightsM = 3_000.0...3_500.0
    /// Located: within this of the truth at every truth point. Confidently wrong: located with
    /// confidence ≥ 0.5 and more than this off at the median truth point.
    private static let maxTruthErrorM = 100.0

    /// `hint_queries` of each map's first search (its far run), as the app stores them for "Search
    /// wider" (`LocateAttempt.hintQueries`), by fixture folder.
    private static var firstSearchQueries: [String: [String]] = [:]

    func testGaiaLangalibalele() async throws {
        try await checkGaia(Self.langalibalele)
    }

    func testGaiaGiantsCastle() async throws {
        try await checkGaia(Self.giantsCastle)
    }

    func testGaiaLangalibaleleSearchWider() async throws {
        try await checkSearchWider(Self.langalibalele)
    }

    func testGaiaGiantsCastleSearchWider() async throws {
        try await checkSearchWider(Self.giantsCastle)
    }

    private func checkGaia(_ map: GaiaMap) async throws {
        let directory = try Fixtures.directory(map.folder)
        let truth = try GaiaCase.load(from: directory.appendingPathComponent("case.json"))

        // 1. The PDF as the importer renders it: attached whole and as a 1:1 crop, and compared with
        //    the fixture's two stand-ins for it.
        let renderStarted = Date()
        let (image, dpi) = try MapImporter.renderBaseImage(from: directory.appendingPathComponent(map.pdf))
        let renderSeconds = Date().timeIntervalSince(renderStarted)
        // Render pixels per map.jpg pixel (case.json is in map.jpg pixels).
        let k = (dpi ?? 300) / 300
        let crop = CGRect(x: map.crop.minX * k, y: map.crop.minY * k, width: map.crop.width * k,
                          height: map.crop.height * k).integral
        if let part = image.cropping(to: crop) {
            attachPNG(part, name: "\(map.folder)-render-crop.png")
        }
        let render = Self.renderCheck(image, truth: truth, scale: k, directory: directory)
        if let render {
            attachJSON(render, name: "\(map.folder)-render-check.json")
        }

        // 2. Apple Vision, as the app runs it.
        let vision = try await visionOCR(map.folder, image)
        let ocr = vision.observations
        attachJSON(VisionOCRFile(description: Self.visionDescription(pdf: map.pdf, pixels: Self.gaiaPixels),
                                 sheet: map.folder, system: Self.system,
                                 imageWidth: image.width, imageHeight: image.height, dpi: dpi, seconds: vision.seconds,
                                 observations: ocr),
                   name: "\(map.folder)-vision-ocr.json")

        // 3. The first search (the app's default radius), from the map centre, then from where the
        //    user's other maps are, with the terrain there and no hints.
        let radiusKm = AppSettings.defaultSearchRadiusKm
        let mapTiles = directory.appendingPathComponent("dem")
        let nearTerrain = try Fixtures.offlineDEMService(tilesIn: mapTiles)
        let centre = await locateGaia(map, run: "centre-dem", image: image, dpi: dpi, ocr: ocr, gps: truth.centre,
                                      dem: nearTerrain, terrain: "fixture tiles (\(map.folder)/dem), no network",
                                      radiusKm: radiusKm, truth: truth, scale: k)
        if (centre.report.truthError?.max ?? .infinity) > Self.maxTruthErrorM {
            // Only when it misses: ~9 MB on the published CI branch otherwise.
            attachPNG(image, name: "\(map.folder)-render.png")
        }
        let farTerrain = try Fixtures.offlineDEMService(tilesInAll: Self.magaliesbergTiles() + [mapTiles])
        let far = await locateGaia(map, run: "far-magaliesberg", image: image, dpi: dpi, ocr: ocr,
                                   gps: Self.magaliesbergGPS, dem: farTerrain,
                                   terrain: "fixture tiles of the Magaliesberg sheets (grootkloof, castle-gorge, "
                                       + "mhlabatini) and \(map.folder)/dem, no network",
                                   radiusKm: radiusKm, truth: truth, scale: k)
        Self.firstSearchQueries[map.folder] = far.outcome.hintQueries

        // 4. What the first search offers to look up, and the labels read off the map.
        let queries = far.outcome.hintQueries
        let cleaned = HintGeocoder.cleanQueries(queries)
        let named = queries.filter { q in Self.expectedNames.contains { Self.letters(q).contains($0) } }
        let centreLabels = centre.outcome.pois.map {
            LabelHeight(name: $0.name, kind: $0.kind.rawValue, elevationM: $0.elevationM)
        }
        let farLabels = (far.outcome.response?.pois ?? []).map {
            LabelHeight(name: $0.name, kind: $0.kind, elevationM: $0.elevationM)
        }
        let furniture = Self.furniture(queries + centreLabels.map(\.name) + farLabels.map(\.name))

        var line = "\(map.folder): render \(image.width)×\(image.height)"
        if let render {
            line += " \(render.upsampling) (equal neighbours \(Self.format(render.equalNeighboursX)), raster "
            line += "\(Self.format(render.nativeEqualNeighboursX)); MAE \(Self.format(render.meanAbsDiffSmoothFixture)) "
            line += "vs map.jpg, \(Self.format(render.meanAbsDiffNearestFixture)) vs map_nearest.jpg)"
        }
        line += ", Vision \(ocr.count) boxes in \(String(format: "%.1f", vision.seconds)) s"
        for report in [centre.report, far.report] {
            line += " | " + Self.describe(report)
        }
        line += " | hint queries \(Array(cleaned.prefix(8)))"
        let summary = GaiaSummary(map: map.folder, outcome: line, system: Self.system,
                                  renderSize: [image.width, image.height], dpi: dpi, renderSeconds: renderSeconds,
                                  render: render, visionBoxes: ocr.count, visionSeconds: vision.seconds,
                                  hintQueries: queries, cleanedHintQueries: cleaned, expectedNamesFound: named,
                                  furniture: furniture,
                                  centreHeights: Self.heightLines(centreLabels), farHeights: Self.heightLines(farLabels),
                                  runs: [centre.report, far.report])
        attachJSON(summary, name: "\(map.folder)-summary.json")
        note(line)

        XCTAssertFalse(ocr.isEmpty, "Vision read nothing on \(map.pdf)")

        // From the map centre: located from the contours, within 100 m at every truth point.
        XCTAssertEqual(centre.report.status, "ok", "centre-dem: \(centre.outcome.message)")
        let g = try XCTUnwrap(centre.outcome.georef, "centre-dem: not located: \(centre.outcome.message)")
        XCTAssertEqual(g.method, .contours, "centre-dem: \(centre.outcome.message)")
        let error = try XCTUnwrap(centre.report.truthError)
        XCTAssertLessThanOrEqual(error.max, Self.maxTruthErrorM, "centre-dem: \(error.text)")

        // From 432 km away, the map beyond the search: no confident wrong answer, and when it is not
        // located the result screen leads to "Search wider".
        XCTAssertFalse(far.report.confidentlyWrong,
                       "far-magaliesberg: confidently wrong: \(far.report.truthError?.text ?? "-") — \(far.outcome.message)")
        if far.outcome.georef == nil {
            XCTAssertTrue(far.outcome.failure?.offersWiderSearch ?? false,
                          "far-magaliesberg: \(far.outcome.failure?.rawValue ?? "no failure") — \(far.outcome.message)")
        }

        // The names "Search wider" looks up: the places, not the page's furniture.
        XCTAssertFalse(named.isEmpty, "hint_queries name neither Giant's Castle nor Langalibalele: \(queries)")
        XCTAssertEqual(furniture, [], "attribution or projection text taken for a place: \(furniture)")
        // Heights printed in feet, given in metres (as the core reads them with and without terrain).
        for (run, labels) in [("centre-dem", centreLabels), ("far-magaliesberg", farLabels)] {
            let heights = Self.heights(labels)
            XCTAssertTrue(heights.allSatisfy { $0 <= Self.maxHeightM },
                          "\(run): heights in feet taken for metres: \(Self.heightLines(labels))")
            XCTAssertTrue(heights.contains { Self.summitHeightsM.contains($0) },
                          "\(run): no summit height (10 000–11 000 ft) among \(Self.heightLines(labels))")
        }
    }

    /// "Search wider" after the first search from the Magaliesberg, as the app runs it: the sheet
    /// looks up the first names on the map and suggests a distance, Start runs the wide search with
    /// the first search's `hint_queries` (`LocateAttempt`), looking them up and checking the places
    /// found first.
    private func checkSearchWider(_ map: GaiaMap) async throws {
        let directory = try Fixtures.directory(map.folder)
        let truth = try GaiaCase.load(from: directory.appendingPathComponent("case.json"))
        let (image, dpi) = try MapImporter.renderBaseImage(from: directory.appendingPathComponent(map.pdf))
        let k = (dpi ?? 300) / 300
        let ocr = try await visionOCR(map.folder, image).observations

        // What the first search stored (testGaia… ran it; run alone, one grid-only core call lists
        // the same names: they come from the text alone).
        let queries: [String]
        if let stored = Self.firstSearchQueries[map.folder] {
            queries = stored
        } else {
            queries = try Self.coreHintQueries(image: image, dpi: dpi, ocr: ocr, gps: Self.magaliesbergGPS)
        }
        let top = try XCTUnwrap(HintGeocoder.cleanQueries(queries).first, "no names on the map to look up: \(queries)")
        let printed = try XCTUnwrap(Self.printedPosition(of: top, in: ocr), "“\(top)” is in no box Vision read")
        let place = try XCTUnwrap(truth.latLon(atPixel: printed, scale: k), "no truth fit")
        let geocoder = TopNameGeocoder(name: top, place: GeocodedPlace(name: top, coordinate: place,
                                                                       area: "KwaZulu-Natal, South Africa"))

        // The sheet: the first names looked up, the distance picked to reach them.
        let lookup = HintGeocoder(geocoder: geocoder, isOnline: { true })
        let named = await lookup.namedPlaces(for: queries, near: Self.magaliesbergGPS, limit: WideSearch.namesLookedUpInSheet)
        let suggested = WideSearch.suggestedRadiusKm(placeDistancesKm: named.compactMap(\.distanceKm),
                                                     after: AppSettings.defaultSearchRadiusKm, units: .metric)
        let radiusKm = suggested ?? 500
        let estimate = MapMeCore.searchEstimate(radiusKm: radiusKm, gps: Self.magaliesbergGPS)
        let options = WideSearchOptions(useMapNames: true, knownHintQueries: queries,
                                        tileCap: WideSearch.tileCap(for: estimate))

        // Start.
        let terrain = try Fixtures.offlineDEMService(tilesInAll: Self.magaliesbergTiles()
            + [directory.appendingPathComponent("dem")])
        let wide = await locateGaia(map, run: "search-wider", image: image, dpi: dpi, ocr: ocr, gps: Self.magaliesbergGPS,
                                    dem: terrain,
                                    terrain: "fixture tiles of the Magaliesberg sheets and \(map.folder)/dem, no network",
                                    radiusKm: radiusKm, truth: truth, scale: k, wide: options, geocoder: geocoder)

        let fromCentreKm = Geodesy.distance(place, truth.centre) / 1000
        let line = "\(map.folder) Search wider: top name “\(top)” printed at (\(Int(printed.x)), \(Int(printed.y))) = "
            + String(format: "%.5f, %.5f, %.1f km from the map centre; ", place.lat, place.lon, fromCentreKm)
            + "sheet suggests \(suggested.map { "\(Int($0)) km" } ?? "nothing") | " + Self.describe(wide.report)
        let report = SearchWiderReport(
            map: map.folder, outcome: line, hintQueries: queries, topName: top,
            printedAtPx: [Double(printed.x), Double(printed.y)], place: place, placeFromMapCentreKm: fromCentreKm,
            placeFromGPSKm: Geodesy.distance(place, Self.magaliesbergGPS) / 1000,
            namedInSheet: named.map { NamedPlaceEntry(name: $0.name, lat: $0.coordinate.lat, lon: $0.coordinate.lon,
                                                      distanceKm: $0.distanceKm) },
            suggestedRadiusKm: suggested, estimateTiles: estimate?.tiles, tileCap: options.tileCap,
            lookedUp: geocoder.queries, run: wide.report)
        attachJSON(report, name: "\(map.folder)-search-wider.json")
        note(line)

        XCTAssertEqual(suggested, 500, "the Search wider sheet should pick 500 km for a place 432 km away: \(named)")
        XCTAssertTrue(geocoder.queries.contains(top), "“\(top)” was not looked up: \(geocoder.queries)")
        XCTAssertEqual(wide.outcome.hintsSent, 1, "the place found for “\(top)” should be the one hint: \(wide.outcome.message)")
        let g = try XCTUnwrap(wide.outcome.georef, "not located: \(wide.outcome.message)")
        XCTAssertEqual(g.method, .contours, wide.outcome.message)
        let error = try XCTUnwrap(wide.report.truthError)
        XCTAssertLessThanOrEqual(error.max, Self.maxTruthErrorM, "search-wider: \(error.text)")
    }

    /// The Magaliesberg sheets' terrain tiles (around `magaliesbergGPS`).
    private static func magaliesbergTiles() throws -> [URL] {
        try ["grootkloof", "castle-gorge", "mhlabatini"].map { try Fixtures.directory($0).appendingPathComponent("dem") }
    }

    /// `hint_queries` as the core lists them for this text: every answer carries them, and a
    /// grid-only call needs no terrain.
    private static func coreHintQueries(image: CGImage, dpi: Double?, ocr: [OCRObservation], gps: LatLon) throws -> [String] {
        let session = try GeoreferenceService.makeSession(from: image)
        var request = GeorefRequest()
        request.gps = GPSFix(lat: gps.lat, lon: gps.lon, accuracyM: 10)
        request.ocr = ocr
        request.dpi = dpi
        request.methods = ["grid"]
        return try session.georeference(request).hintQueries ?? []
    }

    /// Where `name` is printed (render pixels): the centre of the box Vision read it in whole, else
    /// of a box holding its longest word ("Langalibalele" of "Langalibalele Pass" printed on two
    /// lines), else of the longest box that is part of it (a clipped "Langalibalel"), else of the
    /// box that starts like one of its words for longest, at least 5 letters (a name the core
    /// mended from a misread).
    private static func printedPosition(of name: String, in ocr: [OCRObservation]) -> CGPoint? {
        let boxes = ocr.filter { $0.bbox.count == 4 }.map { (key: letters($0.text), bbox: $0.bbox) }
        let whole = letters(name)
        guard whole.count >= 3 else { return nil }
        let words = name.split(whereSeparator: { $0.isWhitespace || $0 == "-" })
            .map { letters(String($0)) }
            .filter { $0.count >= 4 }
            .sorted { $0.count > $1.count }
        for key in [whole] + words {
            if let hit = boxes.first(where: { $0.key.contains(key) }) { return boxCentre(hit.bbox) }
        }
        let parts = boxes.filter { $0.key.count >= 5 && whole.contains($0.key) }
        if let hit = parts.max(by: { $0.key.count < $1.key.count }) { return boxCentre(hit.bbox) }
        var best: (length: Int, bbox: [Double])?
        for box in boxes {
            for word in [whole] + words {
                let common = zip(box.key, word).prefix(while: { pair in pair.0 == pair.1 }).count
                if common >= 5, common > (best?.length ?? 0) { best = (length: common, bbox: box.bbox) }
            }
        }
        return best.map { boxCentre($0.bbox) }
    }

    private static func boxCentre(_ b: [Double]) -> CGPoint {
        CGPoint(x: b[0] + b[2] / 2, y: b[1] + b[3] / 2)
    }

    /// A Gaia fixture's case.json (the benchmark case format, fixtures/tools/bench_contours.py): 25
    /// points of the visible map in map.jpg pixels and where they truly are (truth.json).
    private struct GaiaCase: Decodable {
        struct TruthPoint: Decodable {
            var x: Double
            var y: Double
            var lat: Double
            var lon: Double
        }

        /// The centre of the visible map, [lat, lon].
        var gps: [Double]
        var truthPoints: [TruthPoint]
        /// The visible map [x0, y0, x1, y1] in map.jpg pixels (the legend band covers the raster's bottom).
        var mapFramePx: [Double]

        enum CodingKeys: String, CodingKey {
            case gps
            case truthPoints = "truth_points"
            case mapFramePx = "map_frame_px"
        }

        static func load(from url: URL) throws -> GaiaCase {
            let c = try JSONDecoder().decode(GaiaCase.self, from: Data(contentsOf: url))
            guard c.gps.count == 2, c.truthPoints.count >= 3, c.mapFramePx.count == 4 else {
                throw FixtureError.missing("gps, truth_points or map_frame_px in \(url.lastPathComponent)")
            }
            return c
        }

        var centre: LatLon { LatLon(lat: gps[0], lon: gps[1]) }

        /// Ground error (m) of `g` at every truth point, in pixels of a render `k`× map.jpg.
        func errors(of g: Georeference, scale k: Double) -> [Double] {
            truthPoints.map { p in
                Geodesy.distance(g.latLon(forPixel: CGPoint(x: p.x * k, y: p.y * k)), LatLon(lat: p.lat, lon: p.lon))
            }
        }

        /// Where pixel `p` of a render `k`× map.jpg truly is: the affine through the truth points
        /// (they lie on the truth's own affine, so it is that affine).
        func latLon(atPixel p: CGPoint, scale k: Double) -> LatLon? {
            let src = truthPoints.map { SIMD2($0.x * k, $0.y * k) }
            let dst = truthPoints.map { WebMercator.project(LatLon(lat: $0.lat, lon: $0.lon)) }
            guard let h = LeastSquares.affine(from: src, to: dst) else { return nil }
            return WebMercator.unproject(h.apply(SIMD2(Double(p.x), Double(p.y))))
        }
    }

    private struct GaiaRunReport: Codable {
        /// "centre-dem", "far-magaliesberg" or "search-wider".
        var run: String
        var gps: LatLon
        var gpsFromCentreKm: Double
        var terrain: String
        var searchRadiusKm: Double
        /// A "Search wider" run (place names looked up and checked first).
        var wide: Bool
        var status: String
        var method: String?
        /// Why it was not located (`GeorefFailure`), which decides the result screen's buttons.
        var failure: String?
        /// What the app shows.
        var message: String
        /// The core's own message.
        var coreMessage: String?
        var confidence: Double?
        var rmsM: Double?
        var scaleMPerPx: Double?
        /// Ground error (m) at the case's 25 truth points.
        var truthError: ErrorSummary?
        /// Located with confidence ≥ 0.5 and more than 100 m off at the median truth point.
        var confidentlyWrong: Bool
        var seconds: Double
        var timingsMs: [String: Double]?
        /// The core's last search status (stage, radius searched, tiles, hints tried).
        var search: SearchStatus?
        /// Core calls and terrain tiles asked for, handed over and missing.
        var terrainTiles: [String: Int]
        var hintQueries: [String]
        /// Places named on the map sent to the core as hints.
        var hintsSent: Int
        /// The labels read off the map, as the Nearby list gets them (none when not located).
        var poiCount: Int
        var poisByKind: [String: Int]
        var shortHeights: [String]
        var nameFragments: [String]
    }

    private typealias GaiaRun = (outcome: GeorefOutcome, report: GaiaRunReport)

    /// `GeoreferenceService.locate` as the app calls it for a PDF (no contour colour picked), with
    /// Vision's observations handed in: a first search, or with `wide` a "Search wider" run whose
    /// place lookups go to `geocoder`. Attaches the response and the POIs (as the Nearby list shows
    /// them when located, else as the core classified them).
    private func locateGaia(_ map: GaiaMap, run: String, image: CGImage, dpi: Double?, ocr: [OCRObservation],
                            gps: LatLon, dem: DEMService, terrain: String, radiusKm: Double, truth: GaiaCase,
                            scale k: Double, wide: WideSearchOptions? = nil,
                            geocoder: PlaceGeocoding = NoPlaceGeocoder()) async -> GaiaRun {
        let service = GeoreferenceService(dem: dem, geocoder: geocoder, isOnline: { true })
        let fix = CLLocation(coordinate: gps.coordinate, altitude: 1500, horizontalAccuracy: 10, verticalAccuracy: 10,
                             timestamp: Date())
        let started = Date()
        let outcome = await service.locate(image: image, dpi: dpi, gps: fix, contourRGB: nil, searchRadiusKm: radiusKm,
                                           photo: false, knownText: ocr, wide: wide) { _, _ in }
        let seconds = Date().timeIntervalSince(started)
        let response = outcome.response
        var kinds: [String: Int] = [:]
        for poi in outcome.pois { kinds[poi.kind.rawValue, default: 0] += 1 }
        let error = outcome.georef.map { ErrorSummary(truth.errors(of: $0, scale: k)) }
        let confidentlyWrong = (outcome.georef?.confidence ?? 0) >= 0.5 && (error?.median ?? 0) > Self.maxTruthErrorM
        let tally = outcome.tally
        let report = GaiaRunReport(
            run: run, gps: gps, gpsFromCentreKm: Geodesy.distance(gps, truth.centre) / 1000, terrain: terrain,
            searchRadiusKm: radiusKm, wide: wide != nil, status: response?.status ?? "none", method: response?.method,
            failure: outcome.failure?.rawValue, message: outcome.message, coreMessage: response?.message,
            confidence: response?.confidence, rmsM: response?.rmsM, scaleMPerPx: response?.scaleMPerPx,
            truthError: error, confidentlyWrong: confidentlyWrong, seconds: seconds, timingsMs: response?.timingsMs,
            search: response?.search,
            terrainTiles: ["calls": tally.calls, "rounds": tally.rounds, "requested": tally.requested,
                           "fetched": tally.fetched, "missing": tally.missing],
            hintQueries: outcome.hintQueries, hintsSent: outcome.hintsSent, poiCount: outcome.pois.count,
            poisByKind: kinds, shortHeights: Self.shortHeights(outcome.pois),
            nameFragments: Self.nameFragments(outcome.pois, ocr: ocr))
        if let g = outcome.georef {
            attachJSON(Self.nearbyEntries(outcome.pois, georef: g, from: gps), name: "\(map.folder)-\(run)-pois.json")
        } else if let pois = response?.pois {
            attachJSON(pois, name: "\(map.folder)-\(run)-pois.json")
        }
        if let response {
            attachJSON(response, name: "\(map.folder)-\(run)-response.json")
        }
        return (outcome, report)
    }

    /// "run: status method confidence, truth error (failure), POIs, seconds — message".
    private static func describe(_ report: GaiaRunReport) -> String {
        var text = "\(report.run): \(report.status) \(report.method ?? "-")"
        if let c = report.confidence { text += String(format: " %.2f", c) }
        if let e = report.truthError { text += String(format: ", truth median %.1f m, max %.1f m", e.median, e.max) }
        if let f = report.failure { text += " (\(f))" }
        text += ", \(report.poiCount) POIs, \(String(format: "%.1f", report.seconds)) s — \(report.message)"
        return text
    }

    // MARK: Names and heights

    /// A label read off the map: a POI of the Nearby list, or (not located) as the core classified it.
    private struct LabelHeight {
        var name: String
        /// `POIKind` raw value ("peak", "spot_height", …).
        var kind: String
        var elevationM: Double?
    }

    /// Heights of the peaks and spot heights (metres).
    private static func heights(_ labels: [LabelHeight]) -> [Double] {
        labels.filter { $0.kind == POIKind.peak.rawValue || $0.kind == POIKind.spotHeight.rawValue }
            .compactMap { $0.elevationM }
            .filter { $0.isFinite }
    }

    /// "Mount Durnford (peak): 3294 m" for every label with a height.
    private static func heightLines(_ labels: [LabelHeight]) -> [String] {
        labels.compactMap { label in
            guard let e = label.elevationM, e.isFinite else { return nil }
            return "\(label.name) (\(label.kind)): \(Int(e.rounded())) m"
        }
    }

    /// The names that are the page's attribution or projection text.
    private static func furniture(_ names: [String]) -> [String] {
        names.filter { name in
            let key = name.lowercased().filter { $0.isLetter }
            return pageFurniture.contains { key.contains($0) }
        }
    }

    private static func format(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "-" }
        return String(format: "%.2f", value)
    }

    // MARK: Render check

    /// The importer's render of a Gaia page against the fixture's stand-ins for it: map.jpg (the
    /// raster upsampled smoothly, bicubic) and map_nearest.jpg (by pixel repetition), both JPEG
    /// quality 92; and whether the render repeats raster pixels (CoreGraphics honouring the image's
    /// /Interpolate false) or interpolates them.
    private struct RenderCheck: Codable {
        var width: Int
        var height: Int
        /// The visible map compared, [x0, y0, x1, y1] render pixels.
        var region: [Int]
        /// Mean absolute RGB difference (0–255) over the region, every 2nd pixel each way.
        var meanAbsDiffSmoothFixture: Double?
        var meanAbsDiffNearestFixture: Double?
        /// Share of horizontally / vertically adjacent pixels with exactly the same RGB (every 3rd
        /// row / column of the region). Pixel repetition ~3× makes it about 2/3 + 1/3 of the
        /// raster's own (0.69–0.71 on these maps); smooth upsampling keeps it near the raster's
        /// (+0.02 Lanczos … +0.12 bilinear).
        var equalNeighboursX: Double
        var equalNeighboursY: Double
        /// The same in the raster itself (native.png, the part above the legend band).
        var nativeEqualNeighboursX: Double?
        var nativeEqualNeighboursY: Double?
        /// "nearest" (pixel repetition), "smooth" or "unclear".
        var upsampling: String
    }

    private struct PixelRegion {
        var x0: Int
        var y0: Int
        var x1: Int
        var y1: Int
    }

    private static func renderCheck(_ image: CGImage, truth: GaiaCase, scale k: Double, directory: URL) -> RenderCheck? {
        guard let render = RGBAPixels(image) else { return nil }
        let f = truth.mapFramePx
        let inset = 6
        let region = PixelRegion(x0: Int((f[0] * k).rounded()) + inset, y0: Int((f[1] * k).rounded()) + inset,
                                 x1: min(Int((f[2] * k).rounded()) - inset, render.width),
                                 y1: min(Int((f[3] * k).rounded()) - inset, render.height))
        guard region.x1 - region.x0 > 100, region.y1 - region.y0 > 100 else { return nil }
        let eqX = equalShare(render, region, horizontal: true, every: 3)
        let eqY = equalShare(render, region, horizontal: false, every: 3)

        var nativeX: Double?
        var nativeY: Double?
        if let decoded = try? MapImporter.decodeImage(at: directory.appendingPathComponent("native.png")),
           let native = RGBAPixels(decoded), native.width > 10, native.height > 50 {
            // The legend band covers the raster's bottom 35.5 rows.
            let visible = PixelRegion(x0: 2, y0: 2, x1: native.width - 2, y1: native.height - 40)
            nativeX = equalShare(native, visible, horizontal: true, every: 1)
            nativeY = equalShare(native, visible, horizontal: false, every: 1)
        }
        let maeSmooth = meanAbsDiff(render, fixture: directory.appendingPathComponent("map.jpg"), region)
        let maeNearest = meanAbsDiff(render, fixture: directory.appendingPathComponent("map_nearest.jpg"), region)

        let excessX = eqX - (nativeX ?? 0)
        let excessY = eqY - (nativeY ?? 0)
        let upsampling: String
        if min(excessX, excessY) > 0.35 {
            upsampling = "nearest"
        } else if max(excessX, excessY) < 0.2 {
            upsampling = "smooth"
        } else {
            upsampling = "unclear"
        }
        return RenderCheck(width: render.width, height: render.height,
                           region: [region.x0, region.y0, region.x1, region.y1],
                           meanAbsDiffSmoothFixture: maeSmooth, meanAbsDiffNearestFixture: maeNearest,
                           equalNeighboursX: eqX, equalNeighboursY: eqY,
                           nativeEqualNeighboursX: nativeX, nativeEqualNeighboursY: nativeY, upsampling: upsampling)
    }

    /// Share of pixels in `r` whose right (or lower) neighbour has the same RGB, over every
    /// `every`th row (or column).
    private static func equalShare(_ p: RGBAPixels, _ r: PixelRegion, horizontal: Bool, every: Int) -> Double {
        let w = p.width
        var equal = 0
        var total = 0
        p.bytes.withUnsafeBufferPointer { b in
            if horizontal {
                for y in stride(from: r.y0, to: r.y1, by: every) {
                    var i = (y * w + r.x0) * 4
                    for _ in r.x0..<(r.x1 - 1) {
                        if b[i] == b[i + 4] && b[i + 1] == b[i + 5] && b[i + 2] == b[i + 6] { equal += 1 }
                        total += 1
                        i += 4
                    }
                }
            } else {
                for x in stride(from: r.x0, to: r.x1, by: every) {
                    var i = (r.y0 * w + x) * 4
                    for _ in r.y0..<(r.y1 - 1) {
                        let j = i + w * 4
                        if b[i] == b[j] && b[i + 1] == b[j + 1] && b[i + 2] == b[j + 2] { equal += 1 }
                        total += 1
                        i = j
                    }
                }
            }
        }
        return total > 0 ? Double(equal) / Double(total) : .nan
    }

    /// Mean absolute RGB difference (0–255) between `render` and the image at `fixture` over `r`,
    /// every 2nd pixel each way; nil when the fixture is missing or of another size.
    private static func meanAbsDiff(_ render: RGBAPixels, fixture: URL, _ r: PixelRegion) -> Double? {
        guard let decoded = try? MapImporter.decodeImage(at: fixture), let other = RGBAPixels(decoded),
              other.width == render.width, other.height == render.height else { return nil }
        let w = render.width
        var sum = 0
        var count = 0
        render.bytes.withUnsafeBufferPointer { a in
            other.bytes.withUnsafeBufferPointer { b in
                for y in stride(from: r.y0, to: r.y1, by: 2) {
                    for x in stride(from: r.x0, to: r.x1, by: 2) {
                        let i = (y * w + x) * 4
                        let dr = Int(a[i]) - Int(b[i])
                        let dg = Int(a[i + 1]) - Int(b[i + 1])
                        let db = Int(a[i + 2]) - Int(b[i + 2])
                        sum += abs(dr) + abs(dg) + abs(db)
                        count += 3
                    }
                }
            }
        }
        return count > 0 ? Double(sum) / Double(count) : nil
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

    private struct GaiaSummary: Codable {
        var map: String
        /// One line: the render, Vision, per run status, method, error, POIs, time and message, the names.
        var outcome: String
        var system: String
        var renderSize: [Int]
        var dpi: Double?
        var renderSeconds: Double
        var render: RenderCheck?
        var visionBoxes: Int
        var visionSeconds: Double
        /// `hint_queries` of the first search (the far run) as the core lists them, and as the app
        /// looks them up (`HintGeocoder.cleanQueries`).
        var hintQueries: [String]
        var cleanedHintQueries: [String]
        /// Those naming Giant's Castle or Langalibalele.
        var expectedNamesFound: [String]
        /// Hint queries and POI names that are the page's attribution or projection text.
        var furniture: [String]
        /// The heights of the labels of each run, as given (metres).
        var centreHeights: [String]
        var farHeights: [String]
        var runs: [GaiaRunReport]
    }

    /// `<map>-search-wider.json`.
    private struct SearchWiderReport: Codable {
        var map: String
        var outcome: String
        /// The first search's `hint_queries`, handed to the wide search as the app stores them.
        var hintQueries: [String]
        /// The first of them as the app looks them up: the one name the stand-in for Apple Maps knows.
        var topName: String
        /// Where it is printed (render pixels) and truly is: the place returned for it.
        var printedAtPx: [Double]
        var place: LatLon
        var placeFromMapCentreKm: Double
        var placeFromGPSKm: Double
        /// The places the sheet lists ("Named on the map").
        var namedInSheet: [NamedPlaceEntry]
        var suggestedRadiusKm: Double?
        var estimateTiles: Int?
        var tileCap: Int
        /// Every name looked up (the sheet's, then the search's).
        var lookedUp: [String]
        var run: GaiaRunReport
    }

    private struct NamedPlaceEntry: Codable {
        var name: String
        var lat: Double
        var lon: Double
        var distanceKm: Double?
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

    /// Lossless, so the exact pixels the app analyses can be replayed (`georef_cli`).
    private func attachPNG(_ image: CGImage, name: String) {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, "public.png" as CFString, 1, nil)
        else { return }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return }
        let attachment = XCTAttachment(data: data as Data, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

/// Place lookups of a first search: none (it looks nothing up; this keeps Apple Maps out of the tests).
private struct NoPlaceGeocoder: PlaceGeocoding {
    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace] {
        []
    }
}

/// A stand-in for Apple Maps that knows one name: the top name the first search offered, found
/// where it truly is. Every other name is not found. Records what was looked up.
private final class TopNameGeocoder: PlaceGeocoding, @unchecked Sendable {
    let name: String
    let place: GeocodedPlace
    private let lock = NSLock()
    private var asked: [String] = []

    init(name: String, place: GeocodedPlace) {
        self.name = name
        self.place = place
    }

    var queries: [String] { lock.withLock { asked } }

    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace] {
        lock.withLock { asked.append(query) }
        return query.caseInsensitiveCompare(name) == .orderedSame ? [place] : []
    }
}
