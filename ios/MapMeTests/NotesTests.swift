import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers
import XCTest
@testable import MapMe

/// Notes and photos (docs/design/explorer.md, Notes and photos): where and when a photo was taken
/// read from a JPEG written the way cameras write them, the stored copy without any metadata, its
/// thumbnail, persistence through the geo item store, the placement rules and the stacking of
/// pins.
final class NotesTests: XCTestCase {
    private func temporaryDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("notes-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }

    /// A JPEG written by ImageIO with what a phone camera puts in it: GPS, EXIF times, the
    /// orientation tag. Left half red, right half blue.
    private func cameraJPEG(width: Int, height: Int, gps: [CFString: Any]? = nil, exif: [CFString: Any]? = nil,
                            orientation: Int? = nil) throws -> Data {
        let space = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        let ctx = try XCTUnwrap(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                          space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        ctx.setFillColor(red: 1, green: 0, blue: 0, alpha: 1)
        ctx.fill(CGRect(x: 0, y: 0, width: width / 2, height: height))
        ctx.setFillColor(red: 0, green: 0, blue: 1, alpha: 1)
        ctx.fill(CGRect(x: width / 2, y: 0, width: width - width / 2, height: height))
        let image = try XCTUnwrap(ctx.makeImage())
        let data = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil))
        var properties: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: 0.9]
        if let gps { properties[kCGImagePropertyGPSDictionary] = gps }
        if let exif { properties[kCGImagePropertyExifDictionary] = exif }
        if let orientation { properties[kCGImagePropertyOrientation] = orientation }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return data as Data
    }

    /// GPS as cameras write it: magnitudes with N/S and E/W.
    private func gps(_ p: LatLon, direction: Double? = nil) -> [CFString: Any] {
        var gps: [CFString: Any] = [
            kCGImagePropertyGPSLatitude: abs(p.lat),
            kCGImagePropertyGPSLatitudeRef: p.lat < 0 ? "S" : "N",
            kCGImagePropertyGPSLongitude: abs(p.lon),
            kCGImagePropertyGPSLongitudeRef: p.lon < 0 ? "W" : "E",
        ]
        if let direction {
            gps[kCGImagePropertyGPSImgDirection] = direction
            gps[kCGImagePropertyGPSImgDirectionRef] = "T"
        }
        return gps
    }

    private func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    // MARK: - Reading a photo

    func testReadsWhereAndWhenAPhotoWasTaken() throws {
        var location = gps(LatLon(lat: -25.8417, lon: 27.3934), direction: 140.5)
        location[kCGImagePropertyGPSDateStamp] = "2026:06:21"
        location[kCGImagePropertyGPSTimeStamp] = "09:42:00"
        let jpeg = try cameraJPEG(width: 640, height: 480, gps: location,
                                  exif: [kCGImagePropertyExifDateTimeOriginal: "2026:06:21 11:42:00",
                                         kCGImagePropertyExifOffsetTimeOriginal: "+02:00"])
        let prepared = try NotePhotoCodec.prepare(jpeg, directory: try temporaryDirectory(), placement: .manual)
        let position = try XCTUnwrap(prepared.exifPosition, "the photo's GPS position")
        // Southern latitude from the "S" reference; ~1 m.
        XCTAssertEqual(position.lat, -25.8417, accuracy: 1e-5)
        XCTAssertEqual(position.lon, 27.3934, accuracy: 1e-5)
        XCTAssertEqual(prepared.photo.placement, .exif)
        XCTAssertEqual(try XCTUnwrap(prepared.photo.headingDegrees), 140.5, accuracy: 0.01)
        // 11:42 at UTC+2 (or the GPS stamp, 09:42 UTC): the same moment.
        let expected = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-06-21T09:42:00Z"))
        XCTAssertEqual(try XCTUnwrap(prepared.photo.takenAt).timeIntervalSince(expected), 0, accuracy: 1)
    }

    func testAPhotoWithoutGPSHasNoPosition() throws {
        let jpeg = try cameraJPEG(width: 320, height: 240)
        let prepared = try NotePhotoCodec.prepare(jpeg, directory: try temporaryDirectory(), placement: .myLocation)
        XCTAssertNil(prepared.exifPosition)
        XCTAssertNil(prepared.photo.takenAt)
        XCTAssertEqual(prepared.photo.placement, .myLocation, "placed as the caller says")
    }

    func testMetadataReferencesTimesAndJunk() throws {
        func read(_ gps: [CFString: Any]) -> PhotoMetadata {
            var dict: [String: Any] = [:]
            for (k, v) in gps { dict[k as String] = v }
            return PhotoMetadata.read([kCGImagePropertyGPSDictionary as String: dict])
        }
        XCTAssertEqual(read(gps(LatLon(lat: 46.55, lon: -106))).position, LatLon(lat: 46.55, lon: -106))
        // Signed values without references are kept as they are.
        XCTAssertEqual(read([kCGImagePropertyGPSLatitude: -29.1, kCGImagePropertyGPSLongitude: 29.45]).position,
                       LatLon(lat: -29.1, lon: 29.45))
        // (0, 0) means the camera had no fix; out of range is no place.
        XCTAssertNil(read(gps(LatLon(lat: 0, lon: 0))).position)
        XCTAssertNil(read([kCGImagePropertyGPSLatitude: 95.0, kCGImagePropertyGPSLongitude: 10.0]).position)
        XCTAssertNil(read([:]).position)
        XCTAssertEqual(PhotoMetadata.heading(gps: [kCGImagePropertyGPSImgDirection as String: -10.0]), 350)

        let utc = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-12-21T23:05:30Z"))
        let withOffset = PhotoMetadata.date(exif: [kCGImagePropertyExifDateTimeOriginal as String: "2026:12:21 18:05:30",
                                                   kCGImagePropertyExifOffsetTimeOriginal as String: "-05:00"],
                                            gps: [:], tiff: [:])
        XCTAssertEqual(withOffset, utc)
        let fromGPS = PhotoMetadata.date(exif: [kCGImagePropertyExifDateTimeOriginal as String: "2026:12:21 18:05:30"],
                                         gps: [kCGImagePropertyGPSDateStamp as String: "2026:12:21",
                                               kCGImagePropertyGPSTimeStamp as String: "23:05:30.00"],
                                         tiff: [:])
        XCTAssertEqual(fromGPS, utc, "without an offset the GPS stamp (UTC) wins")
        XCTAssertEqual(PhotoMetadata.timeZone(offset: "+05:30")?.secondsFromGMT(), 19_800)
        XCTAssertNil(PhotoMetadata.timeZone(offset: "02:00"))
    }

    // MARK: - The stored copy

    func testStoredPhotoIsSmallerHasNoMetadataAndAThumbnail() throws {
        let directory = try temporaryDirectory()
        let jpeg = try cameraJPEG(width: 3000, height: 1000, gps: gps(LatLon(lat: -25.84, lon: 27.39), direction: 10),
                                  exif: [kCGImagePropertyExifDateTimeOriginal: "2026:06:21 11:42:00"])
        let prepared = try NotePhotoCodec.prepare(jpeg, directory: directory)
        XCTAssertEqual(prepared.photo.pixelWidth, NotePhotoCodec.maxPixelSize)
        XCTAssertEqual(Double(prepared.photo.pixelHeight), 2048.0 / 3, accuracy: 1)
        XCTAssertEqual(prepared.photo.fileName, "\(prepared.photo.id.uuidString).jpg")

        let stored = directory.appendingPathComponent(prepared.photo.fileName)
        let source = try XCTUnwrap(CGImageSourceCreateWithURL(stored as CFURL, nil))
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [String: Any] ?? [:]
        XCTAssertNil(properties[kCGImagePropertyGPSDictionary as String], "the stored photo must not carry a location")
        let exif = properties[kCGImagePropertyExifDictionary as String] as? [String: Any] ?? [:]
        XCTAssertNil(exif[kCGImagePropertyExifDateTimeOriginal as String])
        XCTAssertNil(PhotoMetadata.read(properties).position)
        XCTAssertNil(PhotoMetadata.read(properties).headingDegrees)

        let thumb = try XCTUnwrap(NotePhotoCodec.loadImage(at: directory.appendingPathComponent(prepared.photo.thumbnailFileName)))
        XCTAssertEqual(thumb.width, NotePhotoCodec.thumbnailPixelSize)
        XCTAssertEqual(thumb.height, NotePhotoCodec.thumbnailPixelSize)
    }

    func testStoredPhotoIsUpright() throws {
        // Orientation 6: stored landscape, shown turned 90° clockwise (a portrait phone photo).
        let jpeg = try cameraJPEG(width: 400, height: 200, orientation: 6)
        let prepared = try NotePhotoCodec.prepare(jpeg, directory: try temporaryDirectory())
        XCTAssertEqual(prepared.photo.pixelWidth, 200)
        XCTAssertEqual(prepared.photo.pixelHeight, 400)
    }

    func testUnreadableDataIsRefusedAndLeavesNothing() throws {
        let directory = try temporaryDirectory()
        XCTAssertThrowsError(try NotePhotoCodec.prepare(Data("not a photo".utf8), directory: directory)) { error in
            XCTAssertEqual(error as? NoteError, .unreadablePhoto)
        }
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: directory.path), [])
    }

    // MARK: - Persistence

    @MainActor
    func testPhotoNotesPersistAndOwnTheirFiles() throws {
        let root = try temporaryDirectory()
        let store = GeoItemStore(directory: root)
        let first = try NotePhotoCodec.prepare(try cameraJPEG(width: 800, height: 600, gps: gps(LatLon(lat: -25.8417, lon: 27.3934))),
                                               directory: store.photosDirectory)
        var note = MapNote(position: try XCTUnwrap(first.exifPosition), elevationM: 1385, text: "Waterfall\nDeep pool",
                           photo: first.photo)
        store.save(note)
        store.flush()

        let reloaded = GeoItemStore(directory: root)
        XCTAssertEqual(reloaded.notes, [note])
        XCTAssertEqual(reloaded.note(id: note.id)?.title, "Waterfall")
        XCTAssertTrue(exists(reloaded.photoURL(for: first.photo)))
        XCTAssertTrue(exists(reloaded.thumbnailURL(for: first.photo)))

        // Editing the text keeps the photo.
        note.text = "Waterfall\nToo cold to swim in June"
        reloaded.save(note)
        reloaded.flush()
        XCTAssertTrue(exists(reloaded.photoURL(for: first.photo)))
        XCTAssertEqual(GeoItemStore(directory: root).note(id: note.id)?.text, note.text)

        // Another photo replaces the first one's files.
        let second = try NotePhotoCodec.prepare(try cameraJPEG(width: 600, height: 800), directory: reloaded.photosDirectory,
                                                placement: .myLocation)
        note.photo = second.photo
        reloaded.save(note)
        reloaded.flush()
        XCTAssertFalse(exists(reloaded.photoURL(for: first.photo)))
        XCTAssertFalse(exists(reloaded.thumbnailURL(for: first.photo)))
        XCTAssertTrue(exists(reloaded.photoURL(for: second.photo)))
        XCTAssertEqual(GeoItemStore(directory: root).note(id: note.id)?.photo, second.photo)

        reloaded.deleteNote(note.id)
        reloaded.flush()
        XCTAssertFalse(exists(reloaded.photoURL(for: second.photo)))
        XCTAssertTrue(GeoItemStore(directory: root).notes.isEmpty)
    }

    func testAnUnsavedPhotoIsRemovedWithItsDraft() throws {
        let directory = try temporaryDirectory()
        let jpeg = try cameraJPEG(width: 320, height: 240)
        let dropped = try NotePhotoCodec.prepare(jpeg, directory: directory)
        let droppedURLs = [directory.appendingPathComponent(dropped.photo.fileName),
                           directory.appendingPathComponent(dropped.photo.thumbnailFileName)]
        do {
            let draft = NotePhotoDraft(urls: droppedURLs)
            XCTAssertTrue(droppedURLs.allSatisfy(exists), "\(draft)")
        }
        XCTAssertFalse(droppedURLs.contains(where: exists), "a cancelled photo leaves nothing behind")

        let saved = try NotePhotoCodec.prepare(jpeg, directory: directory)
        let savedURLs = [directory.appendingPathComponent(saved.photo.fileName),
                         directory.appendingPathComponent(saved.photo.thumbnailFileName)]
        do {
            let draft = NotePhotoDraft(urls: savedURLs)
            draft.keep()
        }
        XCTAssertTrue(savedURLs.allSatisfy(exists), "a saved note keeps its photo")
    }

    // MARK: - Placement

    func testTheDroppedPinWinsAndAFarPhotoAsks() throws {
        let pin = LatLon(lat: -25.8405, lon: 27.3920)
        let far = Geodesy.destination(pin, bearing: 90, distance: 2100)
        let choice = NotePlacementChoice.forNew(origin: .pin(pin), hasPhoto: true, photoPosition: far, photoPlacement: .exif,
                                                fix: nil, mapCentre: nil, units: .metric)
        XCTAssertEqual(choice.candidates.map(\.kind), [.pin, .photo])
        XCTAssertEqual(choice.selected, .pin)
        XCTAssertEqual(choice.selectedCandidate?.position, pin)
        XCTAssertEqual(choice.selectedCandidate?.placement, .manual)
        XCTAssertEqual(choice.message, "This photo was taken 2.10 km from the pin. Use its location?")

        let near = NotePlacementChoice.forNew(origin: .pin(pin), hasPhoto: true,
                                              photoPosition: Geodesy.destination(pin, bearing: 0, distance: 20),
                                              fix: nil, mapCentre: nil, units: .metric)
        XCTAssertNil(near.message, "a photo taken 20 m away does not need a question")

        let text = NotePlacementChoice.forNew(origin: .pin(pin), hasPhoto: false, photoPosition: nil,
                                              fix: LatLon(lat: -25.85, lon: 27.40), mapCentre: nil, units: .metric)
        XCTAssertEqual(text.candidates.map(\.kind), [.pin, .me])
        XCTAssertEqual(text.selected, .pin)
    }

    func testTheNoteButtonUsesThePhotosOwnPlaceElseAsks() throws {
        let taken = LatLon(lat: -25.8417, lon: 27.3934)
        let fix = LatLon(lat: -25.8405, lon: 27.3920)
        let centre = LatLon(lat: -25.85, lon: 27.40)

        let exif = NotePlacementChoice.forNew(origin: .here, hasPhoto: true, photoPosition: taken, photoPlacement: .exif,
                                              fix: fix, mapCentre: centre, units: .metric)
        XCTAssertEqual(exif.candidates.map(\.kind), [.photo, .me, .mapCentre])
        XCTAssertEqual(exif.selected, .photo)
        XCTAssertEqual(exif.selectedCandidate?.placement, .exif)
        XCTAssertNil(exif.message)

        let noGPS = NotePlacementChoice.forNew(origin: .here, hasPhoto: true, photoPosition: nil, fix: fix, mapCentre: centre,
                                               units: .metric)
        XCTAssertEqual(noGPS.candidates.map(\.kind), [.me, .mapCentre])
        XCTAssertEqual(noGPS.selected, .me)
        XCTAssertEqual(noGPS.selectedCandidate?.placement, .myLocation)
        XCTAssertEqual(noGPS.message, "This photo has no location. Choose where to pin it.")

        let noFix = NotePlacementChoice.forNew(origin: .here, hasPhoto: true, photoPosition: nil, fix: nil, mapCentre: centre,
                                               units: .metric)
        XCTAssertEqual(noFix.selected, .mapCentre)
        XCTAssertEqual(noFix.selectedCandidate?.position, centre)

        // The camera's position is your fix: offered once, as where you took it.
        let camera = NotePlacementChoice.forNew(origin: .here, hasPhoto: true, photoPosition: fix, photoPlacement: .myLocation,
                                                fix: fix, mapCentre: centre, units: .metric)
        XCTAssertEqual(camera.candidates.map(\.kind), [.photo, .mapCentre])
        XCTAssertEqual(camera.selectedCandidate?.placement, .myLocation)
        XCTAssertEqual(camera.selectedCandidate?.title, "Where you took the photo")

        let nowhere = NotePlacementChoice.forNew(origin: .here, hasPhoto: false, photoPosition: nil, fix: nil, mapCentre: nil,
                                                 units: .metric)
        XCTAssertNil(nowhere.selectedCandidate, "no fix and no map: the note cannot be placed yet")
        XCTAssertNotNil(nowhere.message)
    }

    func testAnExistingNoteStaysOrMovesToYou() {
        let note = MapNote(position: LatLon(lat: -25.8417, lon: 27.3934), text: "Gate")
        let here = NotePlacementChoice.forExisting(note, fix: note.position)
        XCTAssertEqual(here.candidates.map(\.kind), [.current], "already where you are")
        let away = NotePlacementChoice.forExisting(note, fix: LatLon(lat: -25.8405, lon: 27.3920))
        XCTAssertEqual(away.candidates.map(\.kind), [.current, .me])
        XCTAssertEqual(away.selected, .current)
    }

    // MARK: - Pins on the map

    func testPinsWithin40PointsStackBehindTheFirstInOrder() {
        let points = [CGPoint(x: 100, y: 100), CGPoint(x: 130, y: 100), CGPoint(x: 100, y: 139),
                      CGPoint(x: 200, y: 200), CGPoint(x: 141, y: 100)]
        // Newest first: 0 leads 1 (30 pt) and 2 (39 pt); 4 is 41 pt from 0 and 1 is taken.
        XCTAssertEqual(NoteClustering.leaders(points: points, order: [0, 1, 2, 3, 4], radius: 40), [0, 0, 0, 3, 4])
        // The selected note (1) goes first and leads its stack.
        XCTAssertEqual(NoteClustering.leaders(points: points, order: [1, 0, 2, 3, 4], radius: 40), [1, 1, 2, 3, 1])
        XCTAssertEqual(NoteClustering.leaders(points: [CGPoint(x: 1, y: 1)], order: [0], radius: 40), [0])
        XCTAssertEqual(NoteClustering.leaders(points: points, order: [0, 1, 2, 3, 4], radius: 0), [0, 1, 2, 3, 4])
    }

    func testNoteTexts() {
        let note = MapNote(position: LatLon(lat: -25.8417, lon: 27.3934), text: "Waterfall\n\nDeep pool\nPath on the east")
        XCTAssertEqual(note.title, "Waterfall")
        XCTAssertEqual(NoteFormat.body(note), "Deep pool\nPath on the east")
        XCTAssertNil(NoteFormat.body(MapNote(position: note.position, text: "Only a title")))
        XCTAssertEqual(NoteFormat.shareText(note), "\(note.text)\n\n-25.84170, 27.39340")
    }
}
