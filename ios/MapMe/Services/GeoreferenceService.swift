import CoreGraphics
import CoreLocation
import Foundation

enum GeorefStage: Equatable {
    case readingText
    case analysing
    /// Looking up the names on the map with Apple Maps (wide searches).
    case lookingUpPlaces
    case downloadingTerrain(done: Int, total: Int)
    case matchingContours
    /// Where the core's contour search is (its response's `"search"` object).
    case searching(SearchStatus)
    case finished

    var label: String { label(units: .metric) }

    func label(units: DistanceUnits) -> String {
        switch self {
        case .readingText: return "Reading map labels…"
        case .analysing: return "Looking for grid lines…"
        case .lookingUpPlaces: return "Looking up places named on the map…"
        case .downloadingTerrain(let d, let t): return "Downloading terrain \(d)/\(t)…"
        case .matchingContours: return "Matching contour lines to terrain…"
        case .searching(let status):
            switch status.phase {
            case .hints: return "Checking places named on the map…"
            case .near: return "Searching near you…"
            case .wide:
                if let radius = status.radiusM, radius > 0, let reached = status.searchedRadiusM {
                    return SearchText.searched(km: reached / 1000, of: radius / 1000, units: units)
                }
                return "Searching farther away…"
            case .refine: return "Checking a likely match…"
            case .done, .other: return "Matching contour lines to terrain…"
            }
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
    /// Why the map was not located (nil when it was); decides what the result screen offers.
    var failure: GeorefFailure? = nil
    /// Radius of the search in km.
    var radiusKm: Double? = nil
    /// Names on the map worth looking up (`hint_queries`), best first.
    var hintQueries: [String] = []
    /// Places named on the map that were sent to the core as hints.
    var hintsSent = 0
    /// Terrain tiles asked for and handed to the core.
    var tally = TerrainPullLoop.Tally()
}

/// A search beyond the first one ("Search wider").
struct WideSearchOptions: Equatable {
    /// Look up the names on the map with Apple Maps and have the core check those places first.
    var useMapNames = true
    /// The names from an earlier run (`hint_queries`); nil asks the core for them first.
    var knownHintQueries: [String]? = nil
    /// Hard cap on terrain tiles (see `WideSearch.tileCap(for:)`).
    var tileCap = WideSearch.defaultTileCap
}

/// OCR → mapme-core session → DEM download loop → result.
final class GeoreferenceService {
    /// The most tiles the core may list in one `need_dem` answer.
    static let maxTilesPerRound = 64
    static let progressPollInterval: UInt64 = 150_000_000

    private let dem: TerrainTileSource
    private let hintGeocoder: HintGeocoder
    private let lock = NSLock()
    private var activeSession: GeorefEngine?
    private var activeWork: Task<GeorefResponse, Error>?
    private var cancelled = false

    /// - Parameters:
    ///   - dem: terrain tiles (disk cache + network).
    ///   - geocoder: looks up the names on a map for wide searches.
    ///   - isOnline: false skips those lookups.
    init(dem: TerrainTileSource = DEMService.shared, geocoder: PlaceGeocoding = MapKitPlaceGeocoder(),
         isOnline: @escaping @Sendable () -> Bool = { NetworkPathObserver.isOnlineNow }) {
        self.dem = dem
        self.hintGeocoder = HintGeocoder(geocoder: geocoder, isOnline: isOnline)
    }

    /// `progress(stage, fraction 0…1)` may be called from any thread. `photo` marks a photo of a
    /// paper map (lighting normalisation and a projective fit in the core). `knownText` is text
    /// already read off `image` (an earlier run, tests); nil reads it with Vision. `wide` makes it
    /// a wide search: place names are looked up first and the download cap is the search's own.
    func locate(image: CGImage, dpi: Double?, gps: CLLocation?, contourRGB: [Int]?, searchRadiusKm: Double,
                photo: Bool = false, knownText: [OCRObservation]? = nil, wide: WideSearchOptions? = nil,
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
            var failed = GeorefOutcome(georef: nil, message: "MapMe couldn't analyse this map: \(error.localizedDescription)",
                                       pois: [], response: nil)
            failed.failure = .engineError
            failed.ocr = ocr
            return failed
        }
        if isCancelled { return Self.cancelledOutcome }

        // 3. Grid first, then contours; the core pulls DEM tiles as it needs them.
        var request = GeorefRequest()
        request.gps = Self.gpsFix(from: gps)
        request.ocr = ocr
        request.dpi = dpi
        request.contourRgb = contourRGB
        request.searchRadiusM = searchRadiusKm * 1000
        request.methods = ["grid", "contours"]
        request.photo = photo
        request.maxTilesPerRound = Self.maxTilesPerRound
        var outcome = await run(engine: session, request: request, wide: wide, reporter: reporter)
        outcome.ocr = ocr
        return outcome
    }

    /// Steps 3–4 of `locate` on any engine (tests drive it with a fake core): place-name hints for
    /// wide searches, then the core with its `need_dem` rounds answered until it is done.
    func run(engine: GeorefEngine, request base: GeorefRequest, wide: WideSearchOptions?,
             reporter: MonotonicProgress) async -> GeorefOutcome {
        lock.withLock { activeSession = engine }
        defer { lock.withLock { activeSession = nil } }
        var request = base
        let fix = request.gps
        let radiusKm = request.searchRadiusM / 1000
        let isWide = wide != nil
        let state = SearchRunState()
        var calls = 0

        // Places named on the map → hints the core checks first (wide searches only: within
        // 50 km the near search already covers them at full resolution).
        if let wide, wide.useMapNames, !request.ocr.isEmpty {
            var queries = wide.knownHintQueries
            if queries == nil {
                // The core lists the names with any answer; grid only needs no terrain.
                var probe = request
                probe.methods = ["grid"]
                probe.hints = []
                reporter.report(.analysing, 0.37)
                do {
                    let answer = try await runCore(engine, probe, reporter: reporter) { _, _ in (.analysing, 0.37) }
                    calls += 1
                    if answer.georeference != nil {
                        reporter.report(.finished, 1)
                        return Self.outcome(from: answer, hadGPS: fix != nil, searchRadiusKm: radiusKm)
                    }
                    queries = answer.hintQueries ?? []
                } catch {
                    if isCancelled { return Self.cancelledOutcome }
                }
            }
            if isCancelled { return Self.cancelledOutcome }
            if let queries, !queries.isEmpty {
                state.setHintQueries(queries)
                reporter.report(.lookingUpPlaces, 0.38)
                request.hints = await hintGeocoder.hints(for: queries, near: fix.map { LatLon(lat: $0.lat, lon: $0.lon) },
                                                         searchRadiusM: request.searchRadiusM)
                if isCancelled { return Self.cancelledOutcome }
            }
        }

        // The core, answering its need_dem rounds for as long as it makes progress.
        let searchRequest = request
        let firstCall = calls
        var loop = TerrainPullLoop()
        loop.policy.tileCap = wide?.tileCap ?? WideSearch.nearTileCap
        let result: TerrainPullLoop.Result
        do {
            result = try await loop.run(
                call: { index in
                    let isFirst = firstCall + index == 0
                    let response = try await self.runCore(engine, searchRequest, reporter: reporter) { elapsed, p in
                        Self.pollProgress(state.status, isWide: isWide, isFirstCall: isFirst, elapsed: elapsed, coreProgress: p)
                    }
                    state.absorb(response)
                    if let status = response.search {
                        reporter.report(.searching(status), Self.searchFraction(status, isWide: isWide))
                    }
                    return response
                },
                fetch: { tiles in
                    let status = state.status
                    return await self.dem.tiles(tiles, progress: { done, total in
                        reporter.report(.downloadingTerrain(done: done, total: total),
                                        Self.downloadFraction(status, isWide: isWide, done: done, total: total))
                    })
                },
                add: { tile, png in engine.addDEMTile(tile, png: png) },
                markMissing: { tile in engine.markDEMTileMissing(tile) },
                isCancelled: { self.isCancelled })
        } catch {
            if isCancelled { return Self.cancelledOutcome }
            var failed = GeorefOutcome(georef: nil, message: "MapMe couldn't analyse this map: \(error.localizedDescription)",
                                       pois: [], response: nil)
            failed.failure = .engineError
            failed.radiusKm = radiusKm
            failed.hintQueries = state.hintQueries
            return failed
        }
        if result.end == .cancelled || isCancelled { return Self.cancelledOutcome }

        reporter.report(.finished, 1)
        var outcome = Self.outcome(from: result.response, hadGPS: fix != nil, searchRadiusKm: radiusKm,
                                   hintsSent: searchRequest.hints.count, loop: result, isWide: isWide)
        if outcome.hintQueries.isEmpty { outcome.hintQueries = state.hintQueries }
        return outcome
    }

    /// Cancel the running `locate`.
    func cancel() {
        let (session, work) = lock.withLock { () -> (GeorefEngine?, Task<GeorefResponse, Error>?) in
            cancelled = true
            return (activeSession, activeWork)
        }
        session?.cancel()
        work?.cancel()
    }

    private var isCancelled: Bool {
        Task.isCancelled || lock.withLock { cancelled }
    }

    /// One blocking core call on a background thread; `poll(elapsed, core progress)` turns the
    /// core's progress into a stage and an overall fraction meanwhile.
    private func runCore(_ engine: GeorefEngine, _ request: GeorefRequest, reporter: MonotonicProgress,
                         poll: @escaping @Sendable (TimeInterval, Double) -> (GeorefStage, Double)) async throws -> GeorefResponse {
        let work = Task.detached(priority: .userInitiated) { () throws -> GeorefResponse in
            try engine.georeference(request)
        }
        lock.withLock { activeWork = work }
        let started = Date()
        let poller = Task.detached(priority: .utility) {
            while !Task.isCancelled {
                let p = min(max(Double(engine.progress), 0), 1)
                let (stage, fraction) = poll(Date().timeIntervalSince(started), p)
                reporter.report(stage, fraction)
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
            engine.cancel()
            work.cancel()
        }
    }

    // MARK: - Progress

    /// Stage and overall fraction while a core call runs. A near search follows the core's own
    /// progress per call; a wide one the share of its plan done so far.
    private static func pollProgress(_ status: SearchStatus?, isWide: Bool, isFirstCall: Bool, elapsed: TimeInterval,
                                     coreProgress p: Double) -> (GeorefStage, Double) {
        let searching: GeorefStage? = status.flatMap { s in
            s.phase == .done || s.phase == .other ? nil : GeorefStage.searching(s)
        }
        if isWide {
            return (searching ?? .matchingContours, 0.4 + 0.58 * (status?.fraction ?? 0))
        }
        // The first call tries the grid before contours; say so for its first moments.
        if isFirstCall && elapsed < 0.8 { return (.analysing, 0.4 + 0.58 * p) }
        return (searching ?? .matchingContours, 0.4 + 0.58 * p)
    }

    private static func searchFraction(_ status: SearchStatus, isWide: Bool) -> Double {
        isWide ? 0.4 + 0.58 * (status.fraction ?? 0) : 0.4
    }

    private static func downloadFraction(_ status: SearchStatus?, isWide: Bool, done: Int, total: Int) -> Double {
        let part = Double(done) / Double(max(1, total))
        guard isWide else { return 0.4 + 0.15 * part }
        if let status, let planned = status.tilesPlanned, planned > 0 {
            return 0.4 + 0.58 * min(1, Double((status.tilesFetched ?? 0) + done) / Double(planned))
        }
        return 0.4 + 0.58 * (status?.fraction ?? 0)
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

    /// The outcome of a core run. `hadGPS` and `hintsSent` tell whether there was anywhere to
    /// search; `loop` how the terrain downloads went (nil: not through the loop, e.g. tests).
    static func outcome(from response: GeorefResponse?, hadGPS: Bool, searchRadiusKm: Double, hintsSent: Int = 0,
                        loop: TerrainPullLoop.Result? = nil, isWide: Bool = false) -> GeorefOutcome {
        let tally = loop?.tally ?? TerrainPullLoop.Tally()
        let queries = response?.hintQueries ?? []
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
            return GeorefOutcome(georef: georef, message: message, pois: pois, response: response, failure: nil,
                                 radiusKm: searchRadiusKm, hintQueries: queries, hintsSent: hintsSent, tally: tally)
        }
        let failure = Self.failure(for: response, hadPosition: hadGPS || hintsSent > 0, loop: loop)
        let message = failureMessage(failure, response: response, searchRadiusKm: searchRadiusKm, hintsSent: hintsSent,
                                     tally: tally, isWide: isWide)
        return GeorefOutcome(georef: nil, message: message, pois: [], response: response, failure: failure,
                             radiusKm: searchRadiusKm, hintQueries: queries, hintsSent: hintsSent, tally: tally)
    }

    /// Why a run did not locate the map. A terrain miss (`noTerrainMatch`) needs evidence that the
    /// contour search ran: the response's `search` status, or terrain the core asked for.
    static func failure(for response: GeorefResponse?, hadPosition: Bool, loop: TerrainPullLoop.Result?) -> GeorefFailure {
        if loop?.end == .cancelled { return .cancelled }
        guard let response else { return .engineError }
        if response.message == "cancelled" { return .cancelled }
        let tally = loop?.tally ?? TerrainPullLoop.Tally()
        if loop?.cutoff == .tileCap { return .stoppedAtTileLimit }
        if loop?.cutoff == .offline || (tally.requested > 0 && tally.fetched == 0) { return .terrainUnavailable }
        if !hadPosition { return .noPosition }
        if let message = response.message?.lowercased(), message.hasPrefix("no contour lines") { return .noContours }
        if let s = response.search,
           (s.tilesPlanned ?? 0) > 0 || (s.tilesFetched ?? 0) > 0 || (s.searchedRadiusM ?? 0) > 0 || (s.hintsTried ?? 0) > 0 {
            return .noTerrainMatch
        }
        if tally.requested > 0 { return .noTerrainMatch }
        return .other
    }

    /// What the result screen says. The core's own message is used when it explains the failure
    /// (it says what was searched); the app's own words otherwise.
    static func failureMessage(_ failure: GeorefFailure, response: GeorefResponse?, searchRadiusKm: Double,
                               hintsSent: Int, tally: TerrainPullLoop.Tally, isWide: Bool) -> String {
        let core = response?.message?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let radius = SearchText.distance(km: searchRadiusKm, units: .metric)
        switch failure {
        case .noPosition:
            return "No coordinate grid could be read on this map. Turn on location so MapMe can match the contours near you, or calibrate manually."
        case .stoppedAtTileLimit:
            var text = "Stopped after downloading \(SearchText.grouped(tally.requested)) terrain tiles"
            if let reached = response?.search?.searchedRadiusM, reached > 0 {
                text += " (" + SearchText.searched(km: reached / 1000, of: searchRadiusKm, units: .metric).lowercased() + ")"
            }
            return text + ". Try a shorter distance, or calibrate manually."
        case .terrainUnavailable:
            return "Terrain data couldn't be downloaded, so the contour lines couldn't be matched. Check your connection and try again, or calibrate manually."
        case .cancelled:
            return "Cancelled."
        case .engineError:
            return core.isEmpty ? "MapMe couldn't analyse this map." : core
        case .noTerrainMatch:
            if !core.isEmpty { return core }
            if isWide {
                var details = ["searched \(SearchText.grouped(tally.requested)) terrain tiles"]
                if hintsSent > 0 {
                    details.append("tried \(hintsSent) \(hintsSent == 1 ? "place" : "places") named on the map")
                }
                return "No match within \(radius) of you (\(details.joined(separator: "; ")))."
            }
            return "This map's contour lines didn't match the terrain within \(radius) of you. Search wider, or calibrate it manually."
        case .noContours, .other:
            return core.isEmpty ? "MapMe couldn't work out where this map is. Calibrate it manually instead." : core
        }
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

    private static let cancelledOutcome = GeorefOutcome(georef: nil, message: "Cancelled.", pois: [], response: nil,
                                                        failure: .cancelled)

    #if DEBUG
    /// `-simulateTerrainMiss` (UI tests, screenshots): a contour search that found nothing within
    /// `radiusKm`, without running the core.
    static func simulatedTerrainMiss(radiusKm: Double, isWide: Bool) async -> GeorefOutcome {
        try? await Task.sleep(nanoseconds: 600_000_000)
        let message = isWide
            ? "No match within \(SearchText.distance(km: radiusKm, units: .metric)) of you (searched 0 terrain tiles)."
            : "Couldn't match this map's contours to terrain within \(SearchText.distance(km: radiusKm, units: .metric)) of you."
        return GeorefOutcome(georef: nil, message: message, pois: [], response: nil, failure: .noTerrainMatch,
                             radiusKm: radiusKm, hintQueries: ["Grootkloof", "Magaliesberg", "Hartbeespoort"])
    }
    #endif
}

/// What the running search has reported so far (read by the progress poller). Thread-safe.
private final class SearchRunState: @unchecked Sendable {
    private let lock = NSLock()
    private var lastStatus: SearchStatus?
    private var queries: [String] = []

    var status: SearchStatus? { lock.withLock { lastStatus } }
    var hintQueries: [String] { lock.withLock { queries } }

    func setHintQueries(_ q: [String]) {
        lock.withLock { queries = q }
    }

    func absorb(_ response: GeorefResponse) {
        lock.withLock {
            if let s = response.search { lastStatus = s }
            if let q = response.hintQueries, !q.isEmpty { queries = q }
        }
    }
}

/// Forwards progress with a never-decreasing fraction (several core calls and downloads
/// share one progress bar), in order, and nothing after `.finished`. Thread-safe.
final class MonotonicProgress: @unchecked Sendable {
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
