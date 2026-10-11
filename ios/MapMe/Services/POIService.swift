import CoreGraphics
import Foundation

/// Points of interest from OpenStreetMap (Overpass API) inside a map's footprint, cached per map
/// (<map folder>/osm.json, 7 days). Fails silently when offline.
final class POIService {
    static let endpoint = URL(string: "https://overpass-api.de/api/interpreter")!
    static let cacheTTL: TimeInterval = 7 * 24 * 3600
    static let cacheFileName = "osm.json"
    /// Footprints larger than this (degrees) are not queried.
    static let maxSpanDegrees = 1.0

    struct BoundingBox: Codable, Equatable {
        var south: Double
        var west: Double
        var north: Double
        var east: Double

        func isClose(to other: BoundingBox) -> Bool {
            abs(south - other.south) < 1e-4 && abs(west - other.west) < 1e-4
                && abs(north - other.north) < 1e-4 && abs(east - other.east) < 1e-4
        }
    }

    private struct CacheFile: Codable {
        var fetchedAt: Date
        var bbox: BoundingBox
        var pois: [MapPOI]
    }

    struct OverpassResponse: Decodable {
        struct Element: Decodable {
            var type: String
            var id: Int64
            var lat: Double?
            var lon: Double?
            var tags: [String: String]?
        }

        var elements: [Element]
    }

    private let rootURL: URL
    private let session: URLSession

    /// `rootURL` is the map library root (defaults to MapLibrary's).
    init(rootURL: URL? = nil, session: URLSession? = nil) {
        self.rootURL = rootURL ?? MapLibrary.defaultRootURL
        if let session {
            self.session = session
        } else {
            let config = URLSessionConfiguration.default
            config.timeoutIntervalForRequest = 25
            config.timeoutIntervalForResource = 40
            config.urlCache = nil
            self.session = URLSession(configuration: config)
        }
    }

    func fetchOSM(for map: MapDocument) async -> [MapPOI] {
        guard let bbox = Self.boundingBox(of: map) else { return [] }
        let cacheURL = rootURL.appendingPathComponent(map.id.uuidString, isDirectory: true)
            .appendingPathComponent(Self.cacheFileName)
        let cached = Self.readCache(at: cacheURL)
        if let cached, cached.bbox.isClose(to: bbox), Date().timeIntervalSince(cached.fetchedAt) < Self.cacheTTL {
            return cached.pois
        }
        guard let fresh = await download(bbox) else {
            if let cached, cached.bbox.isClose(to: bbox) { return cached.pois }
            return []
        }
        Self.writeCache(CacheFile(fetchedAt: Date(), bbox: bbox, pois: fresh), to: cacheURL)
        return fresh
    }

    private func download(_ bbox: BoundingBox) async -> [MapPOI]? {
        var request = URLRequest(url: Self.endpoint, timeoutInterval: 25)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded; charset=utf-8", forHTTPHeaderField: "Content-Type")
        request.setValue("MapMe/1.0 (iOS)", forHTTPHeaderField: "User-Agent")
        request.httpBody = Self.formBody(query: Self.query(for: bbox))
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else { return nil }
            let decoded = try JSONDecoder().decode(OverpassResponse.self, from: data)
            return Self.pois(from: decoded)
        } catch {
            return nil
        }
    }

    // MARK: - Pure helpers

    /// Footprint of the whole raster (all four corners), nil if not georeferenced or implausible.
    static func boundingBox(of map: MapDocument) -> BoundingBox? {
        guard let georef = map.georef, map.pixelWidth > 0, map.pixelHeight > 0 else { return nil }
        let w = CGFloat(map.pixelWidth)
        let h = CGFloat(map.pixelHeight)
        let corners = [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)]
            .map { georef.latLon(forPixel: $0) }
        let lats = corners.map(\.lat)
        let lons = corners.map(\.lon)
        guard let south = lats.min(), let north = lats.max(), let west = lons.min(), let east = lons.max(),
              south.isFinite, north.isFinite, west.isFinite, east.isFinite,
              north > south, east > west,
              north - south <= maxSpanDegrees, east - west <= maxSpanDegrees,
              south >= -90, north <= 90, west >= -180, east <= 180
        else { return nil }
        return BoundingBox(south: south, west: west, north: north, east: east)
    }

    static func query(for b: BoundingBox) -> String {
        let box = String(format: "%.6f,%.6f,%.6f,%.6f", b.south, b.west, b.north, b.east)
        let filters = [
            #"[natural~"^(peak|saddle|spring|cave_entrance|volcano)$"]"#,
            #"[waterway=waterfall]"#,
            #"[tourism~"^(viewpoint|alpine_hut|wilderness_hut|camp_site|picnic_site)$"]"#,
            #"[amenity~"^(parking|shelter|drinking_water)$"]"#,
            #"[information=guidepost]"#,
        ]
        let statements = filters.map { "node(\(box))\($0);" }.joined()
        return "[out:json][timeout:20];(\(statements));out body 300;"
    }

    static func formBody(query: String) -> Data? {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        guard let encoded = query.addingPercentEncoding(withAllowedCharacters: allowed) else { return nil }
        return Data(("data=" + encoded).utf8)
    }

    static func pois(from response: OverpassResponse) -> [MapPOI] {
        var seen = Set<Int64>()
        var result: [MapPOI] = []
        for element in response.elements where element.type == "node" {
            guard let lat = element.lat, let lon = element.lon, seen.insert(element.id).inserted else { continue }
            let tags = element.tags ?? [:]
            guard let classified = classify(tags) else { continue }
            let (kind, fallbackName) = classified
            let elevation = tags["ele"].flatMap(parseElevation)
            let name: String
            if let tagged = tags["name"]?.trimmingCharacters(in: .whitespacesAndNewlines), !tagged.isEmpty {
                name = tagged
            } else if let elevation, kind == .peak || fallbackName == "Saddle" {
                name = "\(fallbackName) \(Int(elevation.rounded())) m"
            } else {
                name = fallbackName
            }
            result.append(MapPOI(name: name, kind: kind, source: .openStreetMap,
                                 position: LatLon(lat: lat, lon: lon), elevationM: elevation))
        }
        return result
    }

    /// Kind and a readable default name for an OSM tag set (nil if not one we asked for).
    static func classify(_ tags: [String: String]) -> (POIKind, String)? {
        switch tags["natural"] {
        case "peak": return (.peak, "Peak")
        case "volcano": return (.peak, "Volcano")
        case "saddle": return (.other, "Saddle")
        case "spring": return (.water, "Spring")
        case "cave_entrance": return (.other, "Cave")
        default: break
        }
        if tags["waterway"] == "waterfall" { return (.water, "Waterfall") }
        switch tags["tourism"] {
        case "viewpoint": return (.viewpoint, "Viewpoint")
        case "alpine_hut", "wilderness_hut": return (.shelter, "Hut")
        case "camp_site": return (.shelter, "Campsite")
        case "picnic_site": return (.other, "Picnic site")
        default: break
        }
        switch tags["amenity"] {
        case "parking": return (.parking, "Parking")
        case "shelter": return (.shelter, "Shelter")
        case "drinking_water": return (.water, "Drinking water")
        default: break
        }
        if tags["information"] == "guidepost" { return (.other, "Signpost") }
        return nil
    }

    /// "1523", "1523 m", "1523.4" → metres; feet values ("5000 ft") are converted.
    static func parseElevation(_ text: String) -> Double? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        let number = trimmed.prefix { $0.isNumber || $0 == "." || $0 == "-" }
        guard let value = Double(number), value.isFinite else { return nil }
        let unit = trimmed.dropFirst(number.count).trimmingCharacters(in: .whitespaces).lowercased()
        if unit.hasPrefix("ft") || unit.hasPrefix("'") { return value * 0.3048 }
        return value
    }

    private static func readCache(at url: URL) -> CacheFile? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? MapLibrary.makeDecoder().decode(CacheFile.self, from: data)
    }

    private static func writeCache(_ cache: CacheFile, to url: URL) {
        // Only cache next to a map that still exists.
        guard FileManager.default.fileExists(atPath: url.deletingLastPathComponent().path),
              let data = try? MapLibrary.makeEncoder().encode(cache)
        else { return }
        try? data.write(to: url, options: .atomic)
    }
}
