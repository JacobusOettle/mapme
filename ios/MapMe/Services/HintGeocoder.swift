import CoreLocation
import Foundation
import MapKit

/// A place a name on the map was found at.
struct GeocodedPlace: Equatable, Sendable {
    var name: String
    var coordinate: LatLon
    /// Region and country ("KwaZulu-Natal, South Africa"), when known.
    var area: String? = nil
}

/// Looks up a name printed on a map ("Castle Gorge"). `MapKitPlaceGeocoder` in the app, a mock in
/// tests.
protocol PlaceGeocoding: Sendable {
    /// Places matching `query`, searched around `center` (within about `radiusM`) when known.
    /// Throws (or returns nothing) offline.
    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace]
}

/// Apple Maps search (`MKLocalSearch`): addresses, points of interest and, on iOS 18, physical
/// features (peaks, gorges, rivers), biased to the search circle.
struct MapKitPlaceGeocoder: PlaceGeocoding {
    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace] {
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = query
        var types: MKLocalSearch.ResultType = [.address, .pointOfInterest]
        if #available(iOS 18.0, *) {
            types.insert(.physicalFeature)
        }
        request.resultTypes = types
        if let center {
            let span = min(max(2 * radiusM, 50_000), 4_000_000)
            request.region = MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: center.lat, longitude: center.lon),
                                                latitudinalMeters: span, longitudinalMeters: span)
        }
        let search = MKLocalSearch(request: request)
        let response = try await withTaskCancellationHandler {
            try await search.start()
        } onCancel: {
            search.cancel()
        }
        return response.mapItems.compactMap { item -> GeocodedPlace? in
            let placemark = item.placemark
            let c = placemark.coordinate
            guard CLLocationCoordinate2DIsValid(c), c.latitude != 0 || c.longitude != 0 else { return nil }
            return GeocodedPlace(name: item.name ?? query, coordinate: LatLon(c),
                                 area: Self.area(region: placemark.administrativeArea, country: placemark.country))
        }
    }

    /// "KwaZulu-Natal, South Africa" (either alone when the other is missing or the same).
    static func area(region: String?, country: String?) -> String? {
        var parts: [String] = []
        for part in [region, country] {
            guard let text = part?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty,
                  !parts.contains(text) else { continue }
            parts.append(text)
        }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }
}

/// The app's place lookups: Apple Maps; in Debug builds launched with
/// `-mockPlaces "Name=lat,lon;Name=lat,lon"` (UI tests) a fixed table instead.
enum AppPlaceGeocoder {
    static func make() -> PlaceGeocoding {
        #if DEBUG
        if let table = FixedPlaceGeocoder(launchArguments: ProcessInfo.processInfo.arguments) {
            return table
        }
        #endif
        return MapKitPlaceGeocoder()
    }
}

#if DEBUG
/// Answers lookups from `-mockPlaces "Name=lat,lon;…"` (names match ignoring case).
struct FixedPlaceGeocoder: PlaceGeocoding {
    var table: [String: GeocodedPlace]

    init?(launchArguments args: [String]) {
        guard let flag = args.firstIndex(of: "-mockPlaces"), flag + 1 < args.count else { return nil }
        var table: [String: GeocodedPlace] = [:]
        for entry in args[flag + 1].split(separator: ";") {
            let parts = entry.split(separator: "=", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
            guard parts.count == 2, !parts[0].isEmpty else { continue }
            let numbers = parts[1].split(separator: ",").compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
            guard numbers.count == 2 else { continue }
            table[parts[0].lowercased()] = GeocodedPlace(name: parts[0], coordinate: LatLon(lat: numbers[0], lon: numbers[1]),
                                                         area: "Test place")
        }
        guard !table.isEmpty else { return nil }
        self.table = table
    }

    func places(matching query: String, near center: LatLon?, radiusM: Double) async throws -> [GeocodedPlace] {
        table[query.lowercased()].map { [$0] } ?? []
    }
}
#endif

/// Turns the names the core found on a map (`hint_queries`) into search hints: each name is looked
/// up (a few at a time, each with a timeout), places beyond the search radius of the GPS fix are
/// dropped, and the rest become 15 km circles the core checks first at full resolution.
struct HintGeocoder {
    static let defaultMaxQueries = 8

    var geocoder: PlaceGeocoding
    /// Names looked up (the core lists the best first).
    var maxQueries = HintGeocoder.defaultMaxQueries
    var maxConcurrent = 3
    /// Per lookup; a slow or hanging one is dropped.
    var timeoutSeconds = 4.0
    /// Places kept per name (a name may exist in several places).
    var maxResultsPerQuery = 2
    var maxHints = 12
    var hintRadiusM = SearchHint.defaultRadiusM
    /// Places closer than this to an earlier hint add nothing (their circles overlap).
    var mergeDistanceM = 5_000.0
    /// False skips the lookups (offline).
    var isOnline: @Sendable () -> Bool = { true }

    /// Hints for `queries` around `fix` (nil: anywhere), best first. Never throws: offline, a
    /// failed or slow lookup just gives fewer hints.
    func hints(for queries: [String], near fix: LatLon?, searchRadiusM: Double) async -> [SearchHint] {
        let names = Array(Self.cleanQueries(queries).prefix(maxQueries))
        guard !names.isEmpty, isOnline() else { return [] }
        let results = await lookUpAll(names, near: fix, radiusM: searchRadiusM)
        return Self.hints(from: results, near: fix, searchRadiusM: searchRadiusM,
                          maxResultsPerQuery: maxResultsPerQuery, maxHints: maxHints, hintRadiusM: hintRadiusM,
                          mergeDistanceM: mergeDistanceM)
    }

    /// Where the first `limit` names were found, for the "Search wider" sheet: each name's place
    /// nearest `fix` (anywhere within reach of the widest search) and its distance. Names not
    /// found are left out; offline, nothing.
    func namedPlaces(for queries: [String], near fix: LatLon?, limit: Int) async -> [NamedPlace] {
        let names = Array(Self.cleanQueries(queries).prefix(max(0, limit)))
        guard !names.isEmpty, isOnline() else { return [] }
        let results = await lookUpAll(names, near: fix, radiusM: WideSearch.maxRadiusKm * 1000)
        return WideSearch.namedPlaces(from: results, near: fix)
    }

    /// Looks `names` up a few at a time (`maxConcurrent`), each within `timeoutSeconds`, searching
    /// around `fix` within about `radiusM`; results in the names' order.
    private func lookUpAll(_ names: [String], near fix: LatLon?,
                           radiusM: Double) async -> [(query: String, places: [GeocodedPlace])] {
        let geocoder = self.geocoder
        let timeout = timeoutSeconds
        var found = [[GeocodedPlace]](repeating: [], count: names.count)
        await withTaskGroup(of: (Int, [GeocodedPlace]).self) { group in
            var next = 0
            while next < min(max(1, maxConcurrent), names.count) {
                let index = next
                let name = names[index]
                next += 1
                group.addTask {
                    let places = await Self.lookUp(name, with: geocoder, near: fix, radiusM: radiusM, timeout: timeout)
                    return (index, places)
                }
            }
            while let item = await group.next() {
                found[item.0] = item.1
                guard !Task.isCancelled, next < names.count else { continue }
                let index = next
                let name = names[index]
                next += 1
                group.addTask {
                    let places = await Self.lookUp(name, with: geocoder, near: fix, radiusM: radiusM, timeout: timeout)
                    return (index, places)
                }
            }
        }
        return zip(names, found).map { (query: $0, places: $1) }
    }

    /// Hints from looked-up places, best first: each name's nearest place within `searchRadiusM`
    /// of the fix (any place without a fix), in the names' order, then each name's second, and
    /// so on; places within `mergeDistanceM` of an earlier hint are skipped.
    static func hints(from results: [(query: String, places: [GeocodedPlace])], near fix: LatLon?, searchRadiusM: Double,
                      maxResultsPerQuery: Int = 2, maxHints: Int = 12, hintRadiusM: Double = SearchHint.defaultRadiusM,
                      mergeDistanceM: Double = 5_000) -> [SearchHint] {
        let perQuery: [[(name: String, place: GeocodedPlace)]] = results.map { result -> [(name: String, place: GeocodedPlace)] in
            var places = result.places.filter { $0.coordinate.lat.isFinite && $0.coordinate.lon.isFinite }
            if let fix {
                places = places
                    .map { (place: $0, distance: Geodesy.distance(fix, $0.coordinate)) }
                    .filter { $0.distance <= searchRadiusM }
                    .sorted { $0.distance < $1.distance }
                    .map { $0.place }
            }
            return places.prefix(max(0, maxResultsPerQuery)).map { (name: result.query, place: $0) }
        }
        var hints: [SearchHint] = []
        let depth = perQuery.map(\.count).max() ?? 0
        for rank in 0..<depth {
            for list in perQuery where rank < list.count {
                guard hints.count < maxHints else { return hints }
                let (name, place) = list[rank]
                if hints.contains(where: { Geodesy.distance($0.center, place.coordinate) < mergeDistanceM }) { continue }
                hints.append(SearchHint(lat: place.coordinate.lat, lon: place.coordinate.lon, radiusM: hintRadiusM,
                                        source: .place, name: name))
            }
        }
        return hints
    }

    /// Names worth looking up: trimmed, at least 3 letters, each once (ignoring case). Names in
    /// capitals or in lower case are put in title case ("CASTLE GORGE" → "Castle Gorge"); mixed
    /// case is kept as printed ("KaMasihlenga Pass", "Thaba-Putsoa"), as the "Search wider" sheet
    /// shows them.
    static func cleanQueries(_ queries: [String]) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        for raw in queries {
            let collapsed = raw.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
            let letters = collapsed.filter(\.isLetter)
            guard letters.count >= 3 else { continue }
            let key = collapsed.lowercased()
            guard seen.insert(key).inserted else { continue }
            let mixedCase = letters != letters.uppercased() && letters != letters.lowercased()
            out.append(mixedCase ? collapsed : collapsed.capitalized)
        }
        return out
    }

    /// One lookup; nothing when it fails or takes longer than `timeout` seconds.
    private static func lookUp(_ name: String, with geocoder: PlaceGeocoding, near fix: LatLon?, radiusM: Double,
                               timeout: Double) async -> [GeocodedPlace] {
        let places = await withTimeout(timeout) {
            try await geocoder.places(matching: name, near: fix, radiusM: radiusM)
        }
        return places ?? []
    }

    /// `operation`'s result, or nil when it throws, takes longer than `seconds` or the caller is
    /// cancelled. A late operation is cancelled and given `grace` seconds to wind down, but never
    /// waited on beyond that: a lookup that ignores cancellation (an `MKLocalSearch` whose
    /// completion handler never comes) cannot hold up the search.
    static func withTimeout<T: Sendable>(_ seconds: Double, grace: Double = 1,
                                         _ operation: @escaping @Sendable () async throws -> T) async -> T? {
        let work = Task { () -> T? in try? await operation() }
        let result = await value(of: work, within: seconds)
        work.cancel()
        if let result { return result }
        _ = await value(of: work, within: grace)
        return nil
    }

    /// `.some(work's value)` when it comes within `seconds`; nil when time runs out or the caller is
    /// cancelled first. Never waits longer than that, whatever `work` does.
    private static func value<T: Sendable>(of work: Task<T?, Never>, within seconds: Double) async -> T?? {
        let race = FirstAnswer<T?>()
        let waiter = Task {
            let value = await work.value
            race.answer(.some(value))
        }
        let timer = Task {
            try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
            race.answer(nil)
        }
        let result = await withTaskCancellationHandler {
            await withCheckedContinuation { race.wait($0) }
        } onCancel: {
            race.answer(nil)
        }
        timer.cancel()
        waiter.cancel()
        return result
    }
}

/// The first of several racing answers, handed to one waiting continuation. Thread-safe.
private final class FirstAnswer<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Value?, Never>?
    private var answered = false
    private var value: Value?

    func wait(_ c: CheckedContinuation<Value?, Never>) {
        let ready: Value?? = lock.withLock {
            if answered { return .some(value) }
            continuation = c
            return nil
        }
        if let ready { c.resume(returning: ready) }
    }

    /// The first call wins; later ones are ignored.
    func answer(_ v: Value?) {
        let waiting: CheckedContinuation<Value?, Never>? = lock.withLock {
            guard !answered else { return nil }
            answered = true
            value = v
            defer { continuation = nil }
            return continuation
        }
        waiting?.resume(returning: v)
    }
}
