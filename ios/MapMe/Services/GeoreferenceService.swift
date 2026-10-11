import CoreGraphics
import CoreLocation
import Foundation

enum GeorefStage: Equatable {
    case readingText
    case analysing
    case downloadingTerrain(done: Int, total: Int)
    case matchingContours
    case finished

    var label: String {
        switch self {
        case .readingText: return "Reading map labels…"
        case .analysing: return "Looking for grid lines…"
        case .downloadingTerrain(let d, let t): return "Downloading terrain \(d)/\(t)…"
        case .matchingContours: return "Matching contour lines to terrain…"
        case .finished: return "Done"
        }
    }
}

struct GeorefOutcome {
    var georef: Georeference?
    /// User-facing summary ("Located from grid labels (±2 m)" / reason it failed).
    var message: String
    /// Labels read off the map (place names, peaks), already converted to coordinates.
    var pois: [MapPOI]
    var response: GeorefResponse?
    /// The text the request carried (read with Vision, or handed in), for diagnostics.
    var ocr: [OCRObservation] = []
}

/// OCR → mapme-core session → DEM download loop → result.
final class GeoreferenceService {
    /// DEM download rounds before the core is told to finish with what it has.
    static let maxDEMRounds = 5
    static let progressPollInterval: UInt64 = 150_000_000

    private let dem: DEMService
    private let lock = NSLock()
    private var activeSession: GeorefSession?
    private var activeWork: Task<GeorefResponse, Error>?
    private var cancelled = false

    init(dem: DEMService = .shared) {
        self.dem = dem
    }

    /// `progress(stage, fraction 0…1)` may be called from any thread. `photo` marks a photo of a
    /// paper map (lighting normalisation and a projective fit in the core). `knownText` is text
    /// already read off `image` (tests); nil reads it with Vision.
    func locate(image: CGImage, dpi: Double?, gps: CLLocation?, contourRGB: [Int]?, searchRadiusKm: Double,
                photo: Bool = false, knownText: [OCRObservation]? = nil,
                progress: @escaping (GeorefStage, Double) -> Void) async -> GeorefOutcome {
        lock.withLock { cancelled = false }
        let reporter = MonotonicProgress(progress)

        // 1. Text on the map (grid labels, scale, names). Best effort: contours work without it.
        reporter.report(.readingText, 0.01)
        var ocr: [OCRObservation] = knownText ?? []
        if knownText == nil {
            do {
                ocr = try await OCRService.recognize(in: image) { f in
                    reporter.report(.readingText, 0.01 + 0.34 * f)
                }
            } catch {
                if isCancelled { return Self.cancelledOutcome }
            }
        }
        if isCancelled { return Self.cancelledOutcome }

        // 2. Engine session (the RGBA copy is freed as soon as the core has its own).
        reporter.report(.analysing, 0.36)
        let session: GeorefSession
        do {
            session = try await Task.detached(priority: .userInitiated) {
                try GeoreferenceService.makeSession(from: image)
            }.value
        } catch {
            return GeorefOutcome(georef: nil, message: "MapMe couldn't analyse this map: \(error.localizedDescription)",
                                 pois: [], response: nil)
        }
        lock.withLock { activeSession = session }
        defer { lock.withLock { activeSession = nil } }
        if isCancelled { return Self.cancelledOutcome }

        // 3. Grid first, then contours; the core pulls DEM tiles as it needs them.
        let fix = Self.gpsFix(from: gps)
        var request = GeorefRequest()
        request.gps = fix
        request.ocr = ocr
        request.dpi = dpi
        request.contourRgb = contourRGB
        request.searchRadiusM = searchRadiusKm * 1000
        request.methods = ["grid", "contours"]
        request.photo = photo

        var response: GeorefResponse?
        var downloadRounds = 0
        var gaveUpOnDownloads = false
        var call = 0
        while true {
            if isCancelled { return Self.cancelledOutcome }
            let current: GeorefResponse
            do {
                current = try await runCore(session, request, call: call, reporter: reporter)
            } catch {
                if isCancelled { return Self.cancelledOutcome }
                return GeorefOutcome(georef: nil, message: "MapMe couldn't analyse this map: \(error.localizedDescription)",
                                     pois: [], response: response)
            }
            call += 1
            response = current
            guard current.needsDEM, let needed = current.needTiles, !needed.isEmpty else { break }

            if downloadRounds >= Self.maxDEMRounds {
                if gaveUpOnDownloads { break }
                gaveUpOnDownloads = true
                for t in needed { session.markDEMTileMissing(t) }
                continue
            }
            downloadRounds += 1
            reporter.report(.downloadingTerrain(done: 0, total: needed.count), 0.4)
            let fetched = await dem.tiles(needed) { done, total in
                reporter.report(.downloadingTerrain(done: done, total: total), 0.4 + 0.15 * Double(done) / Double(max(1, total)))
            }
            if isCancelled { return Self.cancelledOutcome }
            for t in needed {
                if let png = fetched[t], session.addDEMTile(t, png: png) {
                    continue
                }
                session.markDEMTileMissing(t)
            }
        }

        reporter.report(.finished, 1)
        var outcome = Self.outcome(from: response, hadGPS: fix != nil, searchRadiusKm: searchRadiusKm)
        outcome.ocr = ocr
        return outcome
    }

    /// Cancel the running `locate`.
    func cancel() {
        let (session, work) = lock.withLock { () -> (GeorefSession?, Task<GeorefResponse, Error>?) in
            cancelled = true
            return (activeSession, activeWork)
        }
        session?.cancel()
        work?.cancel()
    }

    private var isCancelled: Bool {
        Task.isCancelled || lock.withLock { cancelled }
    }

    /// One blocking core call on a background thread, polling its progress meanwhile.
    private func runCore(_ session: GeorefSession, _ request: GeorefRequest, call: Int,
                         reporter: MonotonicProgress) async throws -> GeorefResponse {
        let work = Task.detached(priority: .userInitiated) { () throws -> GeorefResponse in
            try session.georeference(request)
        }
        lock.withLock { activeWork = work }
        let started = Date()
        let poller = Task.detached(priority: .utility) {
            while !Task.isCancelled {
                let p = min(max(Double(session.progress), 0), 1)
                // The first call tries the grid before contours; say so for its first moments.
                let stage: GeorefStage = call == 0 && Date().timeIntervalSince(started) < 0.8 ? .analysing : .matchingContours
                reporter.report(stage, 0.4 + 0.58 * p)
                try? await Task.sleep(nanoseconds: GeoreferenceService.progressPollInterval)
            }
        }
        defer {
            poller.cancel()
            lock.withLock { activeWork = nil }
        }
        return try await withTaskCancellationHandler {
            try await work.value
        } onCancel: {
            session.cancel()
            work.cancel()
        }
    }

    // MARK: - Helpers

    /// Copies the image into an RGBA8 buffer (white under transparency) and hands it to the core.
    static func makeSession(from image: CGImage) throws -> GeorefSession {
        let width = image.width
        let height = image.height
        let bytesPerRow = width * 4
        let byteCount = bytesPerRow * height
        guard width > 0, height > 0 else { throw MapMeCoreError.invalidImage }
        let buffer = UnsafeMutableRawPointer.allocate(byteCount: byteCount, alignment: 16)
        defer { buffer.deallocate() }
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: buffer, width: width, height: height, bitsPerComponent: 8,
                                  bytesPerRow: bytesPerRow, space: space,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { throw MapMeCoreError.invalidImage }
        let rect = CGRect(x: 0, y: 0, width: width, height: height)
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(rect)
        ctx.interpolationQuality = .none
        ctx.draw(image, in: rect)
        return try GeorefSession(rgba: UnsafeRawBufferPointer(start: buffer, count: byteCount),
                                 width: width, height: height, bytesPerRow: bytesPerRow)
    }

    static func gpsFix(from location: CLLocation?) -> GPSFix? {
        guard let location, location.horizontalAccuracy >= 0, CLLocationCoordinate2DIsValid(location.coordinate) else {
            return nil
        }
        return GPSFix(lat: location.coordinate.latitude, lon: location.coordinate.longitude,
                      accuracyM: location.horizontalAccuracy)
    }

    static func outcome(from response: GeorefResponse?, hadGPS: Bool, searchRadiusKm: Double) -> GeorefOutcome {
        if let response, let georef = response.georeference {
            let pois = (response.pois ?? []).compactMap { (p: CorePOI) -> MapPOI? in
                let name = p.name.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty, p.x.isFinite, p.y.isFinite else { return nil }
                return MapPOI(name: name,
                              kind: POIKind(rawValue: p.kind) ?? .other,
                              source: .mapLabel,
                              position: georef.latLon(forPixel: CGPoint(x: p.x, y: p.y)),
                              elevationM: p.elevationM)
            }
            let message = response.message ?? successMessage(georef)
            return GeorefOutcome(georef: georef, message: message, pois: pois, response: response)
        }
        let message: String
        if !hadGPS {
            message = "No coordinate grid could be read on this map. Turn on location so MapMe can match the contours near you, or calibrate manually."
        } else if let text = response?.message, !text.isEmpty {
            message = text
        } else {
            message = "This map's contour lines didn't match the terrain within \(Int(searchRadiusKm.rounded())) km of you. Calibrate it manually instead."
        }
        return GeorefOutcome(georef: nil, message: message, pois: [], response: response)
    }

    static func successMessage(_ georef: Georeference) -> String {
        var accuracy = ""
        if let rms = georef.rmsMeters, rms.isFinite {
            accuracy = " (±\(max(1, Int(rms.rounded()))) m)"
        }
        switch georef.method {
        case .grid: return "Located from the map's coordinate grid\(accuracy)."
        case .labels: return "Located from the map's coordinate labels\(accuracy)."
        case .contours: return "Located by matching contour lines to the terrain\(accuracy)."
        case .manual: return "Located from your control points\(accuracy)."
        }
    }

    private static let cancelledOutcome = GeorefOutcome(georef: nil, message: "Cancelled.", pois: [], response: nil)
}

/// Forwards progress with a never-decreasing fraction (several core calls and downloads
/// share one progress bar), in order, and nothing after `.finished`. Thread-safe.
private final class MonotonicProgress {
    private let lock = NSLock()
    private var last = 0.0
    private var finished = false
    private let callback: (GeorefStage, Double) -> Void

    init(_ callback: @escaping (GeorefStage, Double) -> Void) {
        self.callback = callback
    }

    func report(_ stage: GeorefStage, _ fraction: Double) {
        lock.withLock {
            guard !finished else { return }
            last = max(last, min(max(fraction, 0), 1))
            if stage == .finished { finished = true }
            callback(stage, last)
        }
    }
}
