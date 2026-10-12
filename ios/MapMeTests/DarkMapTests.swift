import CoreGraphics
import XCTest
@testable import MapMe

/// The dark map: the core's dark style through the Swift wrapper, dark tiles in TileStore (no
/// seams between tiles, where the files go, background pre-rendering) and the appearance setting.
final class DarkMapTests: XCTestCase {
    /// The night paper (#212124, `Theme.uiNightPaper`) in 8-bit sRGB.
    private static let nightPaper: (r: Double, g: Double, b: Double) = (33, 33, 36)
    private static let paperColor: (CGFloat, CGFloat, CGFloat) = (0.97, 0.96, 0.93)
    private static let inkColor: (CGFloat, CGFloat, CGFloat) = (0.05, 0.05, 0.05)
    private static let brownColor: (CGFloat, CGFloat, CGFloat) = (0.62, 0.42, 0.22)
    private static let blueColor: (CGFloat, CGFloat, CGFloat) = (0.10, 0.45, 0.85)

    // MARK: - DarkStyle (core wrapper)

    func testDarkStyleOnSyntheticPaperMap() throws {
        let canvas = try Self.paperMap()
        let json = try XCTUnwrap(canvas.withPixels { pixels in
            MapMeCore.darkProfile(rgba: UnsafeRawBufferPointer(pixels), width: canvas.width, height: canvas.height,
                                  bytesPerRow: canvas.bytesPerRow)
        }, "no profile")
        let profile = try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        XCTAssertEqual(profile["status"] as? String, "ok")
        let ink = try XCTUnwrap(profile["ink"] as? Double)
        XCTAssertGreaterThanOrEqual(ink, 0.12)
        XCTAssertLessThan(ink, 0.3, "black print is the darkest ink")
        let paper = try XCTUnwrap(profile["paper"] as? [String: Any])
        let cols = try XCTUnwrap(paper["cols"] as? Int)
        let rows = try XCTUnwrap(paper["rows"] as? Int)
        XCTAssertEqual((paper["rgb"] as? [Double])?.count, cols * rows * 3)

        let style = try XCTUnwrap(DarkStyle(profileJSON: json))
        let rendered = canvas.withPixels { pixels in
            style.render(pixels, width: canvas.width, height: canvas.height, bytesPerRow: canvas.bytesPerRow,
                         u0: 0, v0: 0, du: 1, dv: 1)
        }
        XCTAssertTrue(rendered)

        // Paper becomes the night paper, wherever it is (also between the ink and at the edge).
        for (x, y) in [(400, 300), (5, 5), (90, 76), (795, 595)] {
            Self.assertNightPaper(canvas.pixel(x, y), "paper at \(x),\(y)")
        }
        // Black ink becomes light grey.
        let ink0 = canvas.pixel(90, 64)
        for c in [ink0.r, ink0.g, ink0.b] {
            XCTAssertGreaterThan(Int(c), 195, "ink should be light: \(ink0)")
            XCTAssertLessThan(Int(c), 245, "ink should be light grey, not white: \(ink0)")
        }
        XCTAssertLessThanOrEqual(Int(max(ink0.r, ink0.g, ink0.b)) - Int(min(ink0.r, ink0.g, ink0.b)), 10, "ink stays grey: \(ink0)")
        // Brown contours stay warm (tan), blue water stays blue.
        let brown = canvas.pixel(400, 203)
        XCTAssertGreaterThan(Int(brown.r), 120, "brown: \(brown)")
        XCTAssertGreaterThan(Int(brown.r) - Int(brown.b), 50, "brown stays warm: \(brown)")
        XCTAssertGreaterThan(Int(brown.r), Int(brown.g), "brown stays warm: \(brown)")
        let blue = canvas.pixel(400, 403)
        XCTAssertGreaterThan(Int(blue.b) - Int(blue.r), 50, "blue stays blue: \(blue)")
    }

    func testDarkStyleRejectsInvalidInput() throws {
        XCTAssertNil(DarkStyle(profileJSON: "not json"))
        XCTAssertNil(DarkStyle(profileJSON: "{}"))
        XCTAssertNil(DarkStyle(profileJSON: #"{"status":"failed","message":"no"}"#))
        XCTAssertFalse(MapMeCore.isOKStatus(#"{"status":"failed","message":"no"}"#))
        XCTAssertTrue(MapMeCore.isOKStatus(#"{"status":"ok"}"#))
        // The fallback for maps the core cannot profile is a valid profile.
        let style = try XCTUnwrap(DarkStyle(profileJSON: TileStore.neutralDarkProfile))

        var pixels = [UInt8](repeating: 255, count: 16 * 16 * 4)
        pixels.withUnsafeMutableBytes { raw in
            XCTAssertFalse(style.render(raw, width: 16, height: 16, bytesPerRow: 64, u0: 0, v0: 0, du: 0, dv: 1))
            XCTAssertFalse(style.render(raw, width: 16, height: 16, bytesPerRow: 64, u0: 0, v0: 0, du: 1, dv: -1))
            XCTAssertFalse(style.render(raw, width: 16, height: 16, bytesPerRow: 60, u0: 0, v0: 0, du: 1, dv: 1))
            XCTAssertFalse(style.render(raw, width: 16, height: 17, bytesPerRow: 64, u0: 0, v0: 0, du: 1, dv: 1))
            XCTAssertFalse(style.render(raw, width: 16, height: 16, bytesPerRow: 64, u0: .nan, v0: 0, du: 1, dv: 1))
            XCTAssertTrue(style.render(raw, width: 16, height: 16, bytesPerRow: 64, u0: 0, v0: 0, du: 1, dv: 1))
        }
        // White paper under the neutral profile is the night paper too.
        Self.assertNightPaper((pixels[0], pixels[1], pixels[2]), "neutral profile")
        XCTAssertEqual(pixels[3], 255, "alpha untouched")

        XCTAssertNil(MapMeCore.darkProfile(rgba: UnsafeRawBufferPointer(start: nil, count: 0), width: 0, height: 0, bytesPerRow: 0))
    }

    // MARK: - TileStore dark tiles

    /// A 1024 × 1024 map (2 × 2 tiles at level 0) under uneven light, with lines across the tile
    /// borders: the dark tiles must line up (the paper flattening uses map coordinates, not tile
    /// coordinates), land in tiles-dark-v2 and replace the legacy night tiles.
    func testDarkTilesLineUpAcrossTileBorders() throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let doc = try Self.makeShadedMap(in: library)
        XCTAssertEqual(doc.levels, 2)

        let legacy = library.legacyDarkTilesDirectory(for: doc)
        try FileManager.default.createDirectory(at: legacy.appendingPathComponent("0"), withIntermediateDirectories: true)
        try Data([1, 2, 3]).write(to: legacy.appendingPathComponent("0/0_0.jpg"))

        let store = TileStore(map: doc, library: library)
        var tiles: [String: RGBAPixels] = [:]
        for row in 0..<2 {
            for col in 0..<2 {
                let tile = try XCTUnwrap(store.tile(level: 0, col: col, row: row, dark: true), "\(col)_\(row)")
                XCTAssertEqual(tile.width, 512)
                XCTAssertEqual(tile.height, 512)
                tiles["\(col)_\(row)"] = try XCTUnwrap(RGBAPixels(tile))
                let file = TileStore.tileURL(in: library.tilesDirectory(for: doc, dark: true), level: 0, col: col, row: row)
                XCTAssertTrue(file.path.contains("/tiles-dark-v2/0/"), file.path)
                XCTAssertTrue(FileManager.default.fileExists(atPath: file.path), file.path)
            }
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: legacy.path), "the legacy night tiles should be removed")
        XCTAssertTrue(FileManager.default.fileExists(atPath: library.darkProfileURL(for: doc).path))

        let topLeft = try XCTUnwrap(tiles["0_0"])
        let topRight = try XCTUnwrap(tiles["1_0"])
        let bottomLeft = try XCTUnwrap(tiles["0_1"])
        // The paper is flattened to the night paper on both sides of every border, despite the light.
        for (name, tile) in tiles {
            for (x, y) in [(256, 256), (20, 20), (500, 480), (480, 30)] {
                Self.assertNightPaper(tile.rgb(x, y), "tile \(name) at \(x),\(y)", accuracy: 4)
            }
        }
        // Across a border the picture continues: neighbouring pixel columns (rows) differ no more
        // than neighbouring columns (rows) inside a tile.
        let vertical = Self.meanDifference(512) { y in (topLeft.rgb(511, y), topRight.rgb(0, y)) }
        let horizontal = Self.meanDifference(512) { x in (topLeft.rgb(x, 511), bottomLeft.rgb(x, 0)) }
        let inside = Self.meanDifference(512) { y in (topLeft.rgb(509, y), topLeft.rgb(510, y)) }
        XCTAssertLessThan(vertical, 2.5, "seam between tiles 0_0 and 1_0")
        XCTAssertLessThan(horizontal, 2.5, "seam between tiles 0_0 and 0_1")
        XCTAssertLessThan(vertical, inside + 2)
        // The black line crossing the vertical border is light on both sides.
        XCTAssertGreaterThan(Int(topLeft.rgb(511, 102).g), 150)
        XCTAssertGreaterThan(Int(topRight.rgb(0, 102).g), 150)

        // The coarse level (one tile for the whole map) is drawn with the same flattening.
        let coarse = try XCTUnwrap(RGBAPixels(try XCTUnwrap(store.tile(level: 1, col: 0, row: 0, dark: true))))
        Self.assertNightPaper(coarse.rgb(128, 128), "level 1", accuracy: 4)
        Self.assertNightPaper(coarse.rgb(400, 400), "level 1", accuracy: 4)

        // Served from disk once the memory cache is gone, and from the stored profile by a new store.
        store.purgeMemory()
        let again = TileStore(map: doc, library: library)
        let reloaded = try XCTUnwrap(RGBAPixels(try XCTUnwrap(again.tile(level: 0, col: 1, row: 1, dark: true))))
        Self.assertNightPaper(reloaded.rgb(256, 256), "from disk", accuracy: 4)
        // Light tiles are untouched.
        let light = try XCTUnwrap(RGBAPixels(try XCTUnwrap(again.tile(level: 0, col: 0, row: 0, dark: false))))
        XCTAssertGreaterThan(Int(light.rgb(256, 256).g), 170)
    }

    func testNormalizedTileRectangles() throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let doc = Self.document(width: 1500, height: 900)
        let store = TileStore(map: doc, library: library)
        let a = store.normalizedRect(level: 0, col: 1, row: 1, tileWidth: 512, tileHeight: 388)
        XCTAssertEqual(a.u0, 512.0 / 1500, accuracy: 1e-12)
        XCTAssertEqual(a.v0, 512.0 / 900, accuracy: 1e-12)
        XCTAssertEqual(a.du, 512.0 / 1500, accuracy: 1e-12)
        XCTAssertEqual(a.dv, 388.0 / 900, accuracy: 1e-12)
        XCTAssertEqual(a.v0 + a.dv, 1, accuracy: 1e-12)
        // Level 1: 750 × 450, its pixels two base pixels wide.
        let b = store.normalizedRect(level: 1, col: 1, row: 0, tileWidth: 238, tileHeight: 450)
        XCTAssertEqual(b.u0, 1024.0 / 1500, accuracy: 1e-12)
        XCTAssertEqual(b.u0 + b.du, 1, accuracy: 1e-12)
        XCTAssertEqual(b.v0, 0)
        XCTAssertEqual(b.dv, 1, accuracy: 1e-12)

        // The profile's overview: the largest level with a long side of at most 2048 px.
        XCTAssertEqual(TileStore.overviewLevel(width: 3507, height: 4960, levels: 5), 2)
        XCTAssertEqual(TileStore.overviewLevel(width: 1500, height: 900, levels: 3), 0)
        XCTAssertEqual(TileStore.overviewLevel(width: 4096, height: 3000, levels: 5), 1)
        XCTAssertEqual(TileStore.overviewLevel(width: 9000, height: 9000, levels: 2), 1)
    }

    func testPrerenderDarkWritesEveryTileAndTheThumbnail() throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let doc = try Self.makeShadedMap(in: library)
        let store = TileStore(map: doc, library: library)
        let darkTiles = library.tilesDirectory(for: doc, dark: true)
        let fm = FileManager.default

        store.prerenderDark(shouldStop: { true })
        XCTAssertFalse(fm.fileExists(atPath: darkTiles.path), "a stopped job writes nothing")

        store.prerenderDark(shouldStop: { false })
        for (level, col, row) in [(0, 0, 0), (0, 1, 0), (0, 0, 1), (0, 1, 1), (1, 0, 0)] {
            let url = TileStore.tileURL(in: darkTiles, level: level, col: col, row: row)
            XCTAssertTrue(fm.fileExists(atPath: url.path), url.path)
        }
        let thumb = try XCTUnwrap(TileStore.loadImage(at: library.darkThumbnailURL(for: doc)))
        let light = try XCTUnwrap(TileStore.loadImage(at: library.thumbnailURL(for: doc)))
        XCTAssertEqual(thumb.width, light.width)
        XCTAssertEqual(thumb.height, light.height)
        Self.assertNightPaper(try XCTUnwrap(RGBAPixels(thumb)).rgb(thumb.width / 2, thumb.height / 3), "dark thumbnail",
                              accuracy: 5)

        // A deleted map is never brought back by a late tile.
        try fm.removeItem(at: library.directory(for: doc.id))
        store.prerenderDark(shouldStop: { false })
        XCTAssertNil(store.tile(level: 0, col: 0, row: 0, dark: true))
        XCTAssertFalse(fm.fileExists(atPath: library.directory(for: doc.id).path))
    }

    func testDarkPrerenderJobRunsAndStopsWhenTheMapIsDeleted() async throws {
        let root = try makeTemporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = MapLibrary(rootURL: root)
        let doc = try Self.makeShadedMap(in: library)
        await MainActor.run { library.save(doc) }

        DarkPrerender.start(doc, library: library)
        let deadline = Date().addingTimeInterval(60)
        while DarkPrerender.isRunning(doc.id), Date() < deadline {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertFalse(DarkPrerender.isRunning(doc.id))
        let darkTiles = library.tilesDirectory(for: doc, dark: true)
        XCTAssertTrue(FileManager.default.fileExists(atPath: TileStore.tileURL(in: darkTiles, level: 0, col: 1, row: 1).path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: library.darkThumbnailURL(for: doc).path))

        // Again from scratch, deleting the map while the job runs: everything goes.
        try FileManager.default.removeItem(at: darkTiles)
        DarkPrerender.start(doc, library: library)
        await MainActor.run { library.delete(doc) }
        XCTAssertFalse(DarkPrerender.isRunning(doc.id))
        try await Task.sleep(nanoseconds: 1_500_000_000)
        XCTAssertFalse(FileManager.default.fileExists(atPath: library.directory(for: doc.id).path))
    }

    // MARK: - Setting

    func testMapAppearanceDefaultsToDarkAndPersists() throws {
        let (defaults, suite) = try Self.freshDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        // Users of the old "Night map" switch (off by default) get the dark map too.
        defaults.set(false, forKey: "nightMap")
        let settings = AppSettings(defaults: defaults, arguments: [])
        XCTAssertEqual(settings.mapAppearance, .dark)

        settings.mapAppearance = .paper
        XCTAssertEqual(defaults.string(forKey: "mapAppearance"), "paper")
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: []).mapAppearance, .paper)
        settings.mapAppearance = .dark
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: []).mapAppearance, .dark)
    }

    func testMapAppearanceLaunchArgumentLastsOneRun() throws {
        let (defaults, suite) = try Self.freshDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let overridden = AppSettings(defaults: defaults, arguments: ["MapMe", "-mapAppearance", "paper", "-mockWalk"])
        XCTAssertEqual(overridden.mapAppearance, .paper)
        XCTAssertNil(defaults.string(forKey: "mapAppearance"), "the override is not saved")
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: []).mapAppearance, .dark)

        defaults.set("paper", forKey: "mapAppearance")
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: ["-mapAppearance", "dark"]).mapAppearance, .dark)
        XCTAssertEqual(defaults.string(forKey: "mapAppearance"), "paper")
        // Unknown values and a missing value are ignored.
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: ["-mapAppearance", "sepia"]).mapAppearance, .paper)
        XCTAssertEqual(AppSettings(defaults: defaults, arguments: ["-mapAppearance"]).mapAppearance, .paper)
        XCTAssertEqual(AppSettings.launchValue("-mapAppearance", in: ["x", "-mapAppearance", "dark"]), "dark")
        XCTAssertNil(AppSettings.launchValue("-mapAppearance", in: ["x"]))
    }

    // MARK: - Helpers

    /// Paper with print-like black bars, a brown contour band and a blue stream band (800 × 600).
    private static func paperMap() throws -> SyntheticBitmap {
        let canvas = try SyntheticBitmap(width: 800, height: 600)
        canvas.fill(0, 0, 800, 600, paperColor)
        for i in 0..<10 {
            canvas.fill(60 + (i % 5) * 140, 60 + (i / 5) * 420, 60, 8, inkColor)
        }
        canvas.fill(40, 200, 720, 6, brownColor)
        canvas.fill(40, 400, 720, 6, blueColor)
        return canvas
    }

    /// A 1024 × 1024 map lit unevenly (paper from 76 % to 98 % brightness along the diagonal) with
    /// black lines across the vertical tile border and brown ones across the horizontal border:
    /// pyramid and thumbnail written into `library`, meta not saved.
    private static func makeShadedMap(in library: MapLibrary) throws -> MapDocument {
        let size = 1024
        let canvas = try SyntheticBitmap(width: size, height: size)
        let space = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        // The paper colour (0.98, 0.96, 0.92) at 76 % and at 98 %.
        let dim = try XCTUnwrap(CGColor(colorSpace: space, components: [0.7448, 0.7296, 0.6992, 1]))
        let bright = try XCTUnwrap(CGColor(colorSpace: space, components: [0.9604, 0.9408, 0.9016, 1]))
        let colors = [dim, bright] as CFArray
        let gradient = try XCTUnwrap(CGGradient(colorsSpace: space, colors: colors, locations: [0, 1]))
        // Top-left (dim) to bottom-right (bright); bitmap y runs up.
        canvas.ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: size), end: CGPoint(x: size, y: 0), options: [])
        for y in [100, 300, 700, 900] {
            canvas.fill(40, y, size - 80, 4, inkColor)
        }
        for x in [150, 850] {
            canvas.fill(x, 40, 4, size - 80, brownColor)
        }
        let image = try XCTUnwrap(canvas.ctx.makeImage())
        let doc = document(width: size, height: size)
        let levels = try TileStore.buildPyramid(from: image, into: library.tilesDirectory(for: doc, dark: false))
        XCTAssertEqual(levels, doc.levels)
        try MapImporter.writeThumbnail(of: image, to: library.thumbnailURL(for: doc))
        return doc
    }

    private static func document(width: Int, height: Int) -> MapDocument {
        MapDocument(id: UUID(), name: "Dark test", createdAt: Date(), lastOpenedAt: nil, sourceFileName: "source.png",
                    sourceKind: .image, pixelWidth: width, pixelHeight: height, renderDPI: nil, tileSize: 512,
                    levels: TileStore.levelCount(width: width, height: height, tileSize: 512), georef: nil,
                    georefMessage: nil, pois: [], contourRGB: nil)
    }

    private static func freshDefaults() throws -> (UserDefaults, String) {
        let suite = "MapMeTests.darkMap.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        return (defaults, suite)
    }

    private static func assertNightPaper(_ px: (r: UInt8, g: UInt8, b: UInt8), _ message: String, accuracy: Double = 3,
                                         file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(Double(px.r), nightPaper.r, accuracy: accuracy, "\(message): \(px)", file: file, line: line)
        XCTAssertEqual(Double(px.g), nightPaper.g, accuracy: accuracy, "\(message): \(px)", file: file, line: line)
        XCTAssertEqual(Double(px.b), nightPaper.b, accuracy: accuracy, "\(message): \(px)", file: file, line: line)
    }

    /// Mean absolute RGB difference over `count` pixel pairs.
    private static func meanDifference(_ count: Int,
                                       _ pair: (Int) -> ((r: UInt8, g: UInt8, b: UInt8), (r: UInt8, g: UInt8, b: UInt8))) -> Double {
        var sum = 0
        for i in 0..<count {
            let (a, b) = pair(i)
            sum += abs(Int(a.r) - Int(b.r)) + abs(Int(a.g) - Int(b.g)) + abs(Int(a.b) - Int(b.b))
        }
        return Double(sum) / Double(count * 3)
    }
}

/// An opaque RGBA8 sRGB bitmap (`TileStore.makeContext`) drawn and read with a top-left origin.
private final class SyntheticBitmap {
    let ctx: CGContext
    let width: Int
    let height: Int

    init(width: Int, height: Int) throws {
        self.width = width
        self.height = height
        ctx = try XCTUnwrap(TileStore.makeContext(width: width, height: height))
    }

    var bytesPerRow: Int { ctx.bytesPerRow }

    /// Fills `w` × `h` pixels whose top-left pixel is (x, y).
    func fill(_ x: Int, _ y: Int, _ w: Int, _ h: Int, _ rgb: (CGFloat, CGFloat, CGFloat)) {
        ctx.setFillColor(CGColor(red: rgb.0, green: rgb.1, blue: rgb.2, alpha: 1))
        ctx.fill(CGRect(x: x, y: height - y - h, width: w, height: h))
    }

    /// The bitmap's bytes (rows `bytesPerRow` apart, top row first).
    func withPixels<R>(_ body: (UnsafeMutableRawBufferPointer) -> R) -> R {
        body(UnsafeMutableRawBufferPointer(start: ctx.data, count: ctx.data == nil ? 0 : ctx.bytesPerRow * height))
    }

    func pixel(_ x: Int, _ y: Int) -> (r: UInt8, g: UInt8, b: UInt8) {
        withPixels { (bytes: UnsafeMutableRawBufferPointer) -> (r: UInt8, g: UInt8, b: UInt8) in
            let i = y * bytesPerRow + x * 4
            guard i + 2 < bytes.count else { return (0, 0, 0) }
            return (bytes[i], bytes[i + 1], bytes[i + 2])
        }
    }
}
