import CoreGraphics
import CoreLocation
import Foundation
import Observation
import SwiftUI
import UIKit

/// State of a running (or just finished) import + locate job, shown full-screen
/// by ImportProgressView.
struct ImportJob: Identifiable, Equatable {
    enum Phase: Equatable {
        case importing          // copying, rendering, tiling
        case locating           // OCR, terrain download, matching
        case succeeded          // map located
        case needsCalibration   // imported but could not be located automatically
        case failed             // import itself failed
    }

    var id = UUID()
    var name: String
    var phase: Phase = .importing
    /// Human-readable current step ("Rendering PDF…", "Reading grid labels…").
    var step: String = "Preparing…"
    /// 0…1 overall progress.
    var fraction: Double = 0
    /// Map id once the map exists on disk.
    var mapID: UUID?
    /// Result or error summary.
    var message: String?
    /// Why the map could not be located (with `.needsCalibration`); decides the buttons offered.
    var failure: GeorefFailure?
    /// Radius of the contour search in km.
    var radiusKm: Double?
    /// A "Search wider" run: the progress screen shows how far the search has got.
    var isWideSearch = false
    /// The core's latest search status.
    var search: SearchStatus?
    /// Names on the map worth looking up, for the "Search wider" sheet.
    var hintQueries: [String] = []
}

/// A photo waiting in the corner editor: the import (or re-flattening) is paused until the user
/// confirms the page corners or cancels.
struct PendingPhoto: Identifiable {
    let id = UUID()
    var name: String
    /// The upright photo; corners are in its pixels.
    var image: CGImage
    /// Downscaled copy for display.
    var preview: UIImage
    var initial: PhotoQuad
    /// The page outline found automatically, if any.
    var detected: PhotoQuad?
    /// Re-adjusting an existing map rather than importing a new one.
    var isAdjusting: Bool

    var imageSize: CGSize { CGSize(width: image.width, height: image.height) }
}

@MainActor
@Observable
final class AppModel {
    let settings: AppSettings
    let library: MapLibrary
    let location: LocationService
    let tracks: TrackRecorder

    /// Navigation stack of map ids (LibraryView → MapScreen).
    var path: [UUID] = []
    /// Non-nil while an import/locate job is shown.
    var importJob: ImportJob?
    /// Non-nil while the corner editor is shown for a photo of a paper map.
    var pendingPhoto: PendingPhoto?

    /// The import flow (corner editor or progress) is on screen.
    var isImportFlowActive: Bool { importJob != nil || pendingPhoto != nil }

    @ObservationIgnored private var importTask: Task<Void, Never>?
    @ObservationIgnored private var cornerContinuation: CheckedContinuation<PhotoQuad?, Never>?
    @ObservationIgnored let importer: MapImporter
    @ObservationIgnored let georeferencer: GeoreferenceService
    @ObservationIgnored let poiService: POIService

    init() {
        let library = MapLibrary()
        let location = LocationService()
        let tracks = TrackRecorder()
        self.settings = AppSettings()
        self.library = library
        self.location = location
        self.tracks = tracks
        self.importer = MapImporter(library: library)
        self.georeferencer = GeoreferenceService()
        self.poiService = POIService()
        tracks.attach(location: location)
    }

    /// Called once when the UI appears.
    func onLaunch() async {
        // Hosting unit tests: no location permission alert (it would outlive the unit tests and
        // sit over the UI tests that run next).
        if !location.isIdleForTests {
            location.requestPermission()
            location.start()
        }
        #if DEBUG
        if importSamplePhotoIfRequested() { return }
        #endif
        await importSampleIfRequested()
    }

    // MARK: - Import

    /// A PDF or image arrived from the Files picker, "Open in…", AirDrop or a share sheet.
    /// Photos of paper maps go through the corner editor.
    func handleIncoming(url: URL) {
        let picker = cornerPicker
        startImport(name: url.deletingPathExtension().lastPathComponent) { importer, progress in
            try await importer.importFile(at: url, mode: .detectPhoto(picker), progress: progress)
        }
    }

    /// An image picked from Photos (a screenshot, a saved map or a photo of a paper map).
    func importImage(data: Data, name: String) {
        let picker = cornerPicker
        startImport(name: name) { importer, progress in
            try await importer.importImageData(data, suggestedName: name, mode: .detectPhoto(picker), progress: progress)
        }
    }

    /// A page from the document camera (VisionKit has already flattened it).
    func importScannedPage(_ page: UIImage) {
        let name = "Scan " + Date().formatted(date: .abbreviated, time: .shortened)
        startImport(name: name) { importer, progress in
            progress(0.01, "Saving the scan…")
            let data = try await Task.detached(priority: .userInitiated) { () throws -> Data in
                guard let data = page.jpegData(compressionQuality: MapImporter.flattenedJPEGQuality) else {
                    throw ImportError.unreadable("the scanned page")
                }
                return data
            }.value
            return try await importer.importImageData(data, suggestedName: name, mode: .scannedPage, progress: progress)
        }
    }

    /// Library: "Adjust Corners" on a photo map. Opens the corner editor on the original photo with
    /// the saved corners; Done flattens the page again, rebuilds the tiles and locates the map again.
    /// Cancel leaves the map untouched.
    func adjustCorners(mapID: UUID) {
        guard let map = library.map(id: mapID), map.canAdjustCorners else { return }
        stopRunningImport()
        let job = ImportJob(name: map.name, phase: .importing, step: "Loading the photo…", fraction: 0.02, mapID: map.id)
        importJob = job
        let jobID = job.id
        let importer = self.importer
        let picker = cornerPicker
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let request = try await importer.loadPhoto(for: map)
                guard !Task.isCancelled, self.importJob?.id == jobID else { return }
                guard let corners = await picker(request) else { throw CancellationError() }
                try Task.checkCancellation()
                let imported = try await importer.reflatten(map, photo: request.image, corners: corners) { fraction, step in
                    Task { @MainActor [weak self] in
                        guard let self, self.importJob?.id == jobID else { return }
                        self.importJob?.fraction = 0.4 * fraction
                        self.importJob?.step = step
                    }
                }
                // Tiles and thumbnail changed on disk under the same names.
                TileStore(map: imported.document, library: self.library).purgeMemory()
                MapThumbnailCache.shared.invalidate(self.library.thumbnailURL(for: imported.document))
                MapThumbnailCache.shared.invalidate(self.library.darkThumbnailURL(for: imported.document))
                guard !Task.isCancelled, self.importJob?.id == jobID else { return }
                await self.locate(map: imported.document, image: imported.baseImage)
            } catch is CancellationError {
                // Whoever cancelled the task (Cancel, a newer import, relocate) owns the screen now.
                if !Task.isCancelled, self.importJob?.id == jobID { self.importJob = nil }
            } catch {
                guard self.importJob?.id == jobID else { return }
                self.importJob?.phase = .failed
                self.importJob?.message = error.localizedDescription
            }
        }
    }

    // MARK: - Corner editor

    /// Corner editor: Done. The paused import continues with these corners.
    func confirmCorners(_ corners: PhotoQuad) {
        importJob?.step = "Flattening the map…"
        resolveCorners(corners)
    }

    /// Corner editor: Cancel. Drops the import (an adjusted map stays as it was).
    func cancelCorners() {
        importTask?.cancel()
        importTask = nil
        importJob = nil
        resolveCorners(nil)
    }

    /// Corner editor: "Auto" when nothing was found at import time.
    func detectCorners(in photo: PendingPhoto) async -> PhotoQuad? {
        await PhotoQuadDetector.detect(in: photo.image)
    }

    /// Shows the corner editor and waits for the user; nil when cancelled.
    private var cornerPicker: PhotoCornerPicker {
        { [weak self] request in
            guard let self else { return nil }
            return await self.requestCorners(request)
        }
    }

    private func requestCorners(_ request: PhotoCornerRequest) async -> PhotoQuad? {
        resolveCorners(nil)
        let pending = PendingPhoto(name: request.name, image: request.image, preview: UIImage(cgImage: request.preview),
                                   initial: request.initial, detected: request.detected, isAdjusting: request.isAdjusting)
        return await withCheckedContinuation { (continuation: CheckedContinuation<PhotoQuad?, Never>) in
            cornerContinuation = continuation
            pendingPhoto = pending
        }
    }

    private func resolveCorners(_ corners: PhotoQuad?) {
        let continuation = cornerContinuation
        cornerContinuation = nil
        pendingPhoto = nil
        continuation?.resume(returning: corners)
    }

    /// Cancel the running import/locate task, including one paused in the corner editor.
    private func stopRunningImport() {
        importTask?.cancel()
        resolveCorners(nil)
    }

    /// Run the georeferencer again for an existing map (e.g. after the user picked a contour colour).
    func relocate(mapID: UUID) {
        guard let map = library.map(id: mapID) else { return }
        stopRunningImport()
        // Reuse the visible job's id so the progress cover updates in place instead of re-presenting.
        let job = ImportJob(id: importJob?.id ?? UUID(), name: map.name, phase: .locating, step: "Loading map…",
                            fraction: 0.05, mapID: map.id)
        importJob = job
        let jobID = job.id
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let image = try await self.importer.loadBaseImage(for: map)
                guard !Task.isCancelled, self.importJob?.id == jobID else { return }
                await self.locate(map: map, image: image)
            } catch {
                guard self.importJob?.id == jobID else { return }
                self.importJob?.phase = .needsCalibration
                self.importJob?.message = error.localizedDescription
            }
        }
    }

    /// "Search wider": locate an existing map again within `choice.radiusKm` of you, checking the
    /// places named on the map first when asked. Reuses the base raster and the text read at the
    /// last locate (read again when there is none).
    func searchWider(mapID: UUID, choice: WideSearchChoice) {
        guard let map = library.map(id: mapID) else { return }
        stopRunningImport()
        var job = ImportJob(id: importJob?.id ?? UUID(), name: map.name, phase: .locating, step: "Loading map…",
                            fraction: 0.02, mapID: map.id)
        job.isWideSearch = true
        job.radiusKm = choice.radiusKm
        job.hintQueries = map.locateAttempt?.hintQueries ?? []
        importJob = job
        let jobID = job.id
        let library = self.library
        let storedQueries = map.locateAttempt?.hintQueries ?? []
        let options = WideSearchOptions(useMapNames: choice.useMapNames,
                                        knownHintQueries: storedQueries.isEmpty ? nil : storedQueries,
                                        tileCap: choice.tileCap)
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let image = try await self.importer.loadBaseImage(for: map)
                let ocr = await Task.detached(priority: .userInitiated) { library.loadOCR(for: map) }.value
                guard !Task.isCancelled, self.importJob?.id == jobID else { return }
                await self.locate(map: map, image: image, radiusKm: choice.radiusKm, wide: options, knownText: ocr)
            } catch {
                guard self.importJob?.id == jobID else { return }
                self.importJob?.phase = .needsCalibration
                self.importJob?.message = error.localizedDescription
            }
        }
    }

    func cancelImport() {
        importTask?.cancel()
        georeferencer.cancel()
        importTask = nil
        importJob = nil
        resolveCorners(nil)
    }

    /// Close the progress screen and open the map.
    func finishImport(openMap: Bool) {
        let id = importJob?.mapID
        importJob = nil
        if openMap, let id {
            path = [id]
        }
    }

    private func startImport(name: String, _ work: @escaping (MapImporter, @escaping (Double, String) -> Void) async throws -> ImportedMap) {
        stopRunningImport()
        let job = ImportJob(name: name)
        importJob = job
        let jobID = job.id
        let importer = self.importer
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let imported = try await work(importer) { fraction, step in
                    Task { @MainActor [weak self] in
                        guard let self, self.importJob?.id == jobID else { return }
                        self.importJob?.fraction = 0.4 * fraction
                        self.importJob?.step = step
                    }
                }
                // A newer import replaced this one while it was rendering.
                guard !Task.isCancelled, self.importJob?.id == jobID else { return }
                self.importJob?.mapID = imported.document.id
                self.importJob?.name = imported.document.name
                await self.locate(map: imported.document, image: imported.baseImage)
            } catch is CancellationError {
                // Whoever cancelled the task (Cancel, a newer import, relocate) owns the screen now.
                if !Task.isCancelled, self.importJob?.id == jobID { self.importJob = nil }
            } catch {
                guard self.importJob?.id == jobID else { return }
                self.importJob?.phase = .failed
                self.importJob?.message = error.localizedDescription
            }
        }
    }

    /// OCR + grid/contour matching, then save the result into the library. `radiusKm` is the
    /// setting unless given (the first search); `wide` makes it a "Search wider" run; `knownText`
    /// is the text read off this raster before (nil reads it).
    private func locate(map: MapDocument, image: CGImage, radiusKm: Double? = nil, wide: WideSearchOptions? = nil,
                        knownText: [OCRObservation]? = nil) async {
        let jobID = importJob?.id
        let radius = radiusKm ?? settings.searchRadiusKm
        let units = settings.units
        let isWide = wide != nil
        importJob?.phase = .locating
        importJob?.mapID = map.id
        importJob?.radiusKm = radius
        importJob?.isWideSearch = isWide
        importJob?.search = nil
        importJob?.failure = nil
        let outcome = await runLocate(map: map, image: image, radiusKm: radius, wide: wide,
                                      knownText: knownText) { [weak self] stage, fraction in
            Task { @MainActor [weak self] in
                guard let self, self.importJob != nil, self.importJob?.id == jobID else { return }
                self.importJob?.step = stage.label(units: units)
                // A wide search's bar is the share of its plan done (the service maps it to 0.4…0.98).
                self.importJob?.fraction = isWide ? min(max((fraction - 0.4) / 0.58, 0.02), 1) : 0.4 + 0.6 * fraction
                if case .searching(let status) = stage { self.importJob?.search = status }
            }
        }
        if Task.isCancelled || outcome.failure == .cancelled { return }
        var updated = library.map(id: map.id) ?? map
        updated.georefMessage = outcome.message
        if let g = outcome.georef {
            updated.georef = g
            // Keep user waypoints, replace labels read off the map.
            updated.pois = updated.pois.filter { $0.source != .mapLabel } + outcome.pois
            updated.locateAttempt = nil
        } else {
            let queries = outcome.hintQueries.isEmpty ? (updated.locateAttempt?.hintQueries ?? []) : outcome.hintQueries
            updated.locateAttempt = LocateAttempt(failure: outcome.failure ?? .other, searchedRadiusKm: radius,
                                                  hintQueries: queries)
        }
        library.save(updated)
        if knownText == nil, !outcome.ocr.isEmpty {
            // "Search wider" reads it back instead of running Vision again.
            let cache = OCRCache(pixelWidth: image.width, pixelHeight: image.height, observations: outcome.ocr)
            let url = library.ocrCacheURL(for: map)
            Task.detached(priority: .utility) { try? cache.write(to: url) }
        }
        if importJob != nil, importJob?.id == jobID {
            importJob?.fraction = 1
            importJob?.message = outcome.message
            importJob?.failure = outcome.georef == nil ? (outcome.failure ?? .other) : nil
            importJob?.hintQueries = updated.locateAttempt?.hintQueries ?? outcome.hintQueries
            importJob?.phase = outcome.georef != nil ? .succeeded : .needsCalibration
        }
        if outcome.georef != nil, settings.useOpenStreetMap {
            let snapshot = updated
            Task { [weak self] in
                guard let self else { return }
                let osm = await self.poiService.fetchOSM(for: snapshot)
                guard !osm.isEmpty, var current = self.library.map(id: snapshot.id) else { return }
                current.pois = current.pois.filter { $0.source != .openStreetMap } + osm
                self.library.save(current)
            }
        }
    }

    /// The georeferencer on `map` (in Debug builds `-simulateTerrainMiss` fakes a contour search
    /// that found nothing, for UI tests of "Search wider").
    private func runLocate(map: MapDocument, image: CGImage, radiusKm: Double, wide: WideSearchOptions?,
                           knownText: [OCRObservation]?,
                           progress: @escaping (GeorefStage, Double) -> Void) async -> GeorefOutcome {
        #if DEBUG
        if ProcessInfo.processInfo.arguments.contains("-simulateTerrainMiss") {
            progress(.matchingContours, 0.5)
            return await GeoreferenceService.simulatedTerrainMiss(radiusKm: radiusKm, isWide: wide != nil)
        }
        #endif
        return await georeferencer.locate(image: image, dpi: map.renderDPI, gps: location.location,
                                          contourRGB: map.contourRGB, searchRadiusKm: radiusKm, photo: map.isPhoto,
                                          knownText: knownText, wide: wide, progress: progress)
    }

    // MARK: - UI testing / demo

    #if DEBUG
    /// `-importSamplePhoto` launch argument: import a photo of the sample sheet through the photo
    /// flow, corner editor included. Debug builds bundle the real phone photo of the printed sheet
    /// (SamplePhoto.jpg: wooden table, folded in four); without it the bundled sample map is
    /// photographed synthetically (tilted on a table, uneven light).
    private func importSamplePhotoIfRequested() -> Bool {
        guard ProcessInfo.processInfo.arguments.contains("-importSamplePhoto") else { return false }
        let picker = cornerPicker
        let name = "Grootkloof photo"
        startImport(name: name) { importer, progress in
            progress(0.01, "Loading the sample photo…")
            let photo = try await Task.detached(priority: .userInitiated) {
                try SyntheticPhoto.samplePhoto()
            }.value
            return try await importer.importImageData(photo.jpeg, suggestedName: name,
                                                      mode: .detectPhoto(picker, fallbackCorners: photo.corners),
                                                      progress: progress)
        }
        return true
    }
    #endif

    /// `-importSample` launch argument: import the bundled sample map (Debug builds embed it).
    private func importSampleIfRequested() async {
        let args = ProcessInfo.processInfo.arguments
        guard args.contains("-importSample") else { return }
        // A simulated terrain miss wants the import screen, so it always imports afresh.
        if !args.contains("-simulateTerrainMiss"),
           let existing = library.maps.first(where: { $0.name == "Grootkloof sample" }), existing.isGeoreferenced {
            path = [existing.id]
            return
        }
        guard let url = Bundle.main.url(forResource: "SampleMap", withExtension: "pdf") else { return }
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("Grootkloof sample.pdf")
        try? FileManager.default.removeItem(at: tmp)
        try? FileManager.default.copyItem(at: url, to: tmp)
        handleIncoming(url: tmp)
    }
}
