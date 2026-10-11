import CoreGraphics
import CoreLocation
import Foundation
import Observation
import SwiftUI

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

    let id = UUID()
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
}

@MainActor
@Observable
final class AppModel {
    let settings = AppSettings()
    let library = MapLibrary()
    let location = LocationService()
    let tracks = TrackRecorder()

    /// Navigation stack of map ids (LibraryView → MapScreen).
    var path: [UUID] = []
    /// Non-nil while an import/locate job is shown.
    var importJob: ImportJob?

    @ObservationIgnored private var importTask: Task<Void, Never>?
    @ObservationIgnored let importer: MapImporter
    @ObservationIgnored let georeferencer = GeoreferenceService()
    @ObservationIgnored let poiService = POIService()

    init() {
        importer = MapImporter(library: library)
        tracks.attach(location: location)
    }

    /// Called once when the UI appears.
    func onLaunch() async {
        location.requestPermission()
        location.start()
        await importSampleIfRequested()
    }

    // MARK: - Import

    /// A PDF or image arrived from the Files picker, "Open in…", AirDrop or a share sheet.
    func handleIncoming(url: URL) {
        startImport(name: url.deletingPathExtension().lastPathComponent) { importer, progress in
            try await importer.importFile(at: url, progress: progress)
        }
    }

    /// An image picked from Photos.
    func importImage(data: Data, name: String) {
        startImport(name: name) { importer, progress in
            try await importer.importImageData(data, suggestedName: name, progress: progress)
        }
    }

    /// Run the georeferencer again for an existing map (e.g. after the user picked a contour colour).
    func relocate(mapID: UUID) {
        guard let map = library.map(id: mapID) else { return }
        importTask?.cancel()
        importJob = ImportJob(name: map.name, phase: .locating, step: "Loading map…", fraction: 0.05, mapID: map.id)
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let image = try await self.importer.loadBaseImage(for: map)
                await self.locate(map: map, image: image)
            } catch {
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
        importTask?.cancel()
        importJob = ImportJob(name: name)
        let importer = self.importer
        importTask = Task { [weak self] in
            guard let self else { return }
            do {
                let imported = try await work(importer) { fraction, step in
                    Task { @MainActor [weak self] in
                        guard let self, self.importJob != nil else { return }
                        self.importJob?.fraction = 0.4 * fraction
                        self.importJob?.step = step
                    }
                }
                self.importJob?.mapID = imported.document.id
                self.importJob?.name = imported.document.name
                await self.locate(map: imported.document, image: imported.baseImage)
            } catch is CancellationError {
                self.importJob = nil
            } catch {
                self.importJob?.phase = .failed
                self.importJob?.message = error.localizedDescription
            }
        }
    }

    /// OCR + grid/contour matching, then save the result into the library.
    private func locate(map: MapDocument, image: CGImage) async {
        importJob?.phase = .locating
        importJob?.mapID = map.id
        let outcome = await georeferencer.locate(
            image: image,
            dpi: map.renderDPI,
            gps: location.location,
            contourRGB: map.contourRGB,
            searchRadiusKm: settings.searchRadiusKm
        ) { [weak self] stage, fraction in
            Task { @MainActor [weak self] in
                guard let self, self.importJob != nil else { return }
                self.importJob?.step = stage.label
                self.importJob?.fraction = 0.4 + 0.6 * fraction
            }
        }
        if Task.isCancelled { return }
        var updated = library.map(id: map.id) ?? map
        updated.georefMessage = outcome.message
        if let g = outcome.georef {
            updated.georef = g
            // Keep user waypoints, replace labels read off the map.
            updated.pois = updated.pois.filter { $0.source != .mapLabel } + outcome.pois
        }
        library.save(updated)
        importJob?.fraction = 1
        importJob?.message = outcome.message
        importJob?.phase = outcome.georef != nil ? .succeeded : .needsCalibration
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

    // MARK: - UI testing / demo

    /// `-importSample` launch argument: import the bundled sample map (Debug builds embed it).
    private func importSampleIfRequested() async {
        let args = ProcessInfo.processInfo.arguments
        guard args.contains("-importSample") else { return }
        if let existing = library.maps.first(where: { $0.name == "Grootkloof sample" }), existing.isGeoreferenced {
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
