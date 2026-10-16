import CoreLocation
import Foundation
import ImageIO
import Observation
import UIKit
import UniformTypeIdentifiers

enum NoteError: Error, LocalizedError, Equatable {
    case unreadablePhoto
    case couldNotSave
    case notImplemented

    var errorDescription: String? {
        switch self {
        case .unreadablePhoto: return "This photo couldn't be read."
        case .couldNotSave: return "The photo couldn't be saved. Is your iPhone's storage full?"
        case .notImplemented: return "Notes aren't available yet."
        }
    }
}

/// A photo ready to become a note: re-encoded for the store, with what its metadata said.
struct PreparedPhoto: Sendable {
    var photo: NotePhoto
    /// Where the photo says it was taken (EXIF GPS), if anywhere.
    var exifPosition: LatLon?
}

/// Notes and photos on the map (docs/design/explorer.md, Notes and photos): photos from the
/// camera or the library, re-encoded without metadata into the geo item store, placed by their
/// GPS position or at you; thumbnails for the map. Nothing leaves the device. Use from the main
/// thread; image work runs in the background.
@MainActor
@Observable
final class NoteService {
    /// Bumped when a thumbnail has loaded: `thumbnail(for:)` reads it, so views and overlays that
    /// asked for one redraw when it arrives.
    private(set) var thumbnailRevision = 0

    @ObservationIgnored private weak var model: AppModel?
    /// Decoded thumbnails by photo id (160 px squares: ~100 KB each decoded).
    @ObservationIgnored private var thumbnails: [UUID: UIImage] = [:]
    @ObservationIgnored private var loadingThumbnails: Set<UUID> = []
    /// Thumbnails that could not be read (no file): not tried again this session.
    @ObservationIgnored private var missingThumbnails: Set<UUID> = []

    /// At most this many thumbnails stay decoded (~50 MB would be 500; notes are rarely that many).
    static let thumbnailCacheLimit = 400

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Where the photos live (the geo item store's `photos/`).
    var photosDirectory: URL {
        model?.geoItems.photosDirectory
            ?? GeoItemStore.defaultDirectory.appendingPathComponent("photos", isDirectory: true)
    }

    func photoURL(for photo: NotePhoto) -> URL { photosDirectory.appendingPathComponent(photo.fileName) }

    // MARK: - Photos

    /// Re-encodes `data` (JPEG/HEIC from the camera or the library) into the store's photos folder
    /// (≤ 2048 px, no metadata) with its thumbnail, and reads its GPS position and capture time.
    func preparePhoto(_ data: Data) async throws -> PreparedPhoto {
        let directory = photosDirectory
        return try await Task.detached(priority: .userInitiated) {
            try NotePhotoCodec.prepare(data, directory: directory, placement: .manual)
        }.value
    }

    /// A photo just taken with the camera (no GPS in it: the note uses your fix at capture).
    func preparePhoto(_ image: UIImage) async throws -> PreparedPhoto {
        let directory = photosDirectory
        return try await Task.detached(priority: .userInitiated) {
            // The camera's UIImage carries its orientation; JPEG data keeps it as EXIF, which the
            // codec applies while downsizing.
            guard let data = image.jpegData(compressionQuality: 0.95) else { throw NoteError.unreadablePhoto }
            return try NotePhotoCodec.prepare(data, directory: directory, placement: .myLocation)
        }.value
    }

    /// Removes a prepared photo that no note uses (the editor was cancelled).
    func discard(_ photo: NotePhoto) {
        guard model?.geoItems.notes.contains(where: { $0.photo?.id == photo.id }) != true else { return }
        let urls = [photoURL(for: photo), photosDirectory.appendingPathComponent(photo.thumbnailFileName)]
        thumbnails[photo.id] = nil
        Task.detached(priority: .utility) {
            for url in urls { try? FileManager.default.removeItem(at: url) }
        }
    }

    /// A small square thumbnail for the map (cached in memory), nil until loaded. Reading it
    /// observes `thumbnailRevision`, so the caller redraws once it has loaded.
    func thumbnail(for photo: NotePhoto) -> UIImage? {
        _ = thumbnailRevision
        if let hit = thumbnails[photo.id] { return hit }
        loadThumbnail(photo)
        return nil
    }

    /// The photo itself (≤ 2048 px), decoded off the main thread.
    func image(for photo: NotePhoto) async -> UIImage? {
        let url = photoURL(for: photo)
        return await Task.detached(priority: .userInitiated) { () -> UIImage? in
            NotePhotoCodec.loadImage(at: url).map { UIImage(cgImage: $0) }
        }.value
    }

    private func loadThumbnail(_ photo: NotePhoto) {
        let id = photo.id
        guard !loadingThumbnails.contains(id), !missingThumbnails.contains(id) else { return }
        loadingThumbnails.insert(id)
        let thumbURL = photosDirectory.appendingPathComponent(photo.thumbnailFileName)
        let fullURL = photoURL(for: photo)
        Task { [weak self] in
            let image = await Task.detached(priority: .utility) { () -> UIImage? in
                // A thumbnail lost to a crash is made again from the photo.
                let cg = NotePhotoCodec.loadImage(at: thumbURL)
                    ?? NotePhotoCodec.loadImage(at: fullURL, maxPixelSize: NotePhotoCodec.thumbnailPixelSize)
                return cg.map { UIImage(cgImage: $0) }
            }.value
            guard let self else { return }
            self.loadingThumbnails.remove(id)
            guard let image else {
                self.missingThumbnails.insert(id)
                return
            }
            if self.thumbnails.count >= Self.thumbnailCacheLimit { self.thumbnails.removeAll() }
            self.thumbnails[id] = image
            self.thumbnailRevision += 1
        }
    }

    // MARK: - Notes

    /// Saves a note (new or edited) and looks up its elevation when it has none.
    func save(_ note: MapNote) {
        guard let store = model?.geoItems else { return }
        store.save(note)
        fillElevation(note.id)
    }

    /// Deletes a note and its photo files.
    func delete(_ id: UUID) {
        guard let store = model?.geoItems else { return }
        if let photo = store.note(id: id)?.photo { thumbnails[photo.id] = nil }
        store.deleteNote(id)
    }

    /// The terrain's elevation at `position` (nil without terrain, e.g. offline with nothing
    /// cached there).
    func elevation(at position: LatLon) async -> Double? {
        guard let terrain = model?.terrain else { return nil }
        let answer = await Task.detached(priority: .utility) {
            await terrain.resolve { store in store.elevations(ElevationsRequest(points: [position])) }
        }.value
        guard let value = answer?.value?.elevationsM.first ?? nil, value.isFinite else { return nil }
        return value
    }

    /// Looks up a note's elevation in the background and saves it, unless the note has moved or
    /// changed meanwhile.
    func fillElevation(_ id: UUID) {
        guard let note = model?.geoItems.note(id: id), note.elevationM == nil else { return }
        let position = note.position
        Task { [weak self] in
            guard let self, let elevation = await self.elevation(at: position),
                  var current = self.model?.geoItems.note(id: id),
                  current.position == position, current.elevationM == nil
            else { return }
            current.elevationM = elevation
            self.model?.geoItems.save(current)
        }
    }

    // MARK: - Debug seeding

    #if DEBUG
    /// Notes `-seedExplorerItems` puts on the Grootkloof sample sheet (fixed ids: seeding again
    /// changes nothing).
    static let seededPhotoNoteID = UUID(uuidString: "5EED0000-0000-4000-8000-0000000000A1")!
    static let seededTextNoteID = UUID(uuidString: "5EED0000-0000-4000-8000-0000000000A2")!
    static let seededPhotoTitle = "Waterfall below the cliffs"
    static let seededTextTitle = "Gate locked after 18:00"

    /// A photo note (a generated landscape carrying GPS EXIF, so it goes through the real
    /// re-encoding and placement) and a text note, near the sample's mock position.
    func seedSampleNotes() async {
        guard let store = model?.geoItems else { return }
        if store.note(id: Self.seededTextNoteID) == nil {
            store.save(MapNote(id: Self.seededTextNoteID, position: LatLon(lat: -25.8393, lon: 27.3905),
                               elevationM: 1452, text: "\(Self.seededTextTitle)\nUse the stile on the left of the gate.",
                               createdAt: Date(timeIntervalSinceNow: -3 * 86_400), updatedAt: Date(timeIntervalSinceNow: -3 * 86_400)))
        }
        guard store.note(id: Self.seededPhotoNoteID) == nil else { return }
        let position = LatLon(lat: -25.8417, lon: 27.3934)
        let jpeg = await Task.detached(priority: .userInitiated) { Self.sampleLandscapeJPEG(at: position) }.value
        guard let jpeg, let prepared = try? await preparePhoto(jpeg) else { return }
        // A save in between (a second launch argument run) must not be doubled.
        guard store.note(id: Self.seededPhotoNoteID) == nil else {
            discard(prepared.photo)
            return
        }
        store.save(MapNote(id: Self.seededPhotoNoteID, position: prepared.exifPosition ?? position, elevationM: 1385,
                           text: "\(Self.seededPhotoTitle)\nPool deep enough to swim in; the path down is on the east side.",
                           createdAt: Date(timeIntervalSinceNow: -86_400), updatedAt: Date(timeIntervalSinceNow: -86_400),
                           photo: prepared.photo))
    }

    /// A 1200 × 900 drawn landscape (sky, two ridges, a waterfall) as a camera JPEG with GPS EXIF.
    nonisolated static func sampleLandscapeJPEG(at position: LatLon) -> Data? {
        let size = CGSize(width: 1200, height: 900)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let image = UIGraphicsImageRenderer(size: size, format: format).image { ctx in
            let cg = ctx.cgContext
            let space = CGColorSpaceCreateDeviceRGB()
            let sky = [UIColor(red: 0.36, green: 0.62, blue: 0.92, alpha: 1).cgColor,
                       UIColor(red: 0.98, green: 0.80, blue: 0.56, alpha: 1).cgColor] as CFArray
            if let gradient = CGGradient(colorsSpace: space, colors: sky, locations: [0, 1]) {
                cg.drawLinearGradient(gradient, start: .zero, end: CGPoint(x: 0, y: size.height * 0.7), options: [])
            }
            UIColor(red: 1.0, green: 0.93, blue: 0.70, alpha: 1).setFill()
            UIBezierPath(ovalIn: CGRect(x: 860, y: 120, width: 120, height: 120)).fill()
            let far = UIBezierPath()
            far.move(to: CGPoint(x: 0, y: 520))
            far.addLine(to: CGPoint(x: 260, y: 300))
            far.addLine(to: CGPoint(x: 470, y: 430))
            far.addLine(to: CGPoint(x: 700, y: 260))
            far.addLine(to: CGPoint(x: 1200, y: 480))
            far.addLine(to: CGPoint(x: 1200, y: 900))
            far.addLine(to: CGPoint(x: 0, y: 900))
            far.close()
            UIColor(red: 0.42, green: 0.47, blue: 0.56, alpha: 1).setFill()
            far.fill()
            let near = UIBezierPath()
            near.move(to: CGPoint(x: 0, y: 640))
            near.addCurve(to: CGPoint(x: 1200, y: 600), controlPoint1: CGPoint(x: 380, y: 520),
                          controlPoint2: CGPoint(x: 820, y: 720))
            near.addLine(to: CGPoint(x: 1200, y: 900))
            near.addLine(to: CGPoint(x: 0, y: 900))
            near.close()
            UIColor(red: 0.22, green: 0.38, blue: 0.24, alpha: 1).setFill()
            near.fill()
            UIColor(white: 0.97, alpha: 0.9).setFill()
            UIBezierPath(roundedRect: CGRect(x: 560, y: 330, width: 46, height: 380), cornerRadius: 20).fill()
            UIColor(red: 0.55, green: 0.78, blue: 0.92, alpha: 1).setFill()
            UIBezierPath(ovalIn: CGRect(x: 470, y: 680, width: 230, height: 80)).fill()
        }
        guard let cg = image.cgImage else { return nil }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil)
        else { return nil }
        let gps: [CFString: Any] = [
            kCGImagePropertyGPSLatitude: abs(position.lat),
            kCGImagePropertyGPSLatitudeRef: position.lat < 0 ? "S" : "N",
            kCGImagePropertyGPSLongitude: abs(position.lon),
            kCGImagePropertyGPSLongitudeRef: position.lon < 0 ? "W" : "E",
            kCGImagePropertyGPSImgDirection: 140.0,
            kCGImagePropertyGPSImgDirectionRef: "T",
        ]
        let exif: [CFString: Any] = [kCGImagePropertyExifDateTimeOriginal: "2026:06:20 11:42:00",
                                     kCGImagePropertyExifOffsetTimeOriginal: "+02:00"]
        let properties: [CFString: Any] = [kCGImagePropertyGPSDictionary: gps, kCGImagePropertyExifDictionary: exif,
                                           kCGImageDestinationLossyCompressionQuality: 0.9]
        CGImageDestinationAddImage(destination, cg, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }
    #endif
}

/// Notes and photos, and the battery saver are one builder's; this is the notes half's hooks.
@MainActor
enum NotesFeature {
    static func attach(_ model: AppModel) {
        model.notes.attach(model)
    }

    /// Debug launch arguments: `-seedExplorerItems` (a note with a generated photo near the sample).
    static func appDidLaunch(_ model: AppModel) async {
        #if DEBUG
        if ProcessInfo.processInfo.arguments.contains("-seedExplorerItems") {
            await model.notes.seedSampleNotes()
        }
        #endif
    }
}
