import CoreTransferable
import Foundation
import Observation
import UniformTypeIdentifiers

/// What a GPX file held.
struct GPXDocument: Equatable, Sendable {
    /// `<wpt>`s.
    var waypoints: [Waypoint] = []
    /// `<rte>`s (kind `.gpxRoute`) and `<trk>`s (kind `.gpxTrack`, one route per track, its
    /// segments joined).
    var routes: [Route] = []
    /// `<metadata><name>`, if any.
    var name: String? = nil
    /// The `creator` attribute ("GaiaGPS", "AllTrails", "MapMe").
    var creator: String? = nil
    /// Points left out: coordinates missing or outside ±90°/±180°.
    var skippedPoints = 0

    var isEmpty: Bool { waypoints.isEmpty && routes.isEmpty }
}

/// The outcome of one import, for the summary sheet.
struct GPXImportSummary: Identifiable, Equatable, Sendable {
    var id = UUID()
    var fileName: String
    var waypointIDs: [UUID] = []
    var routeIDs: [UUID] = []
    /// Set when the file could not be read (and nothing was imported).
    var error: String? = nil
    /// The bounds of everything imported (to show it: the best map covering it, or the world view).
    var bounds: GeoBounds? = nil
    /// The same file (name and content) was imported before, at this date: nothing is imported
    /// until the user picks Replace or Keep Both.
    var duplicateImportedAt: Date? = nil
    /// Still reading the file.
    var isLoading = false
    /// The routes, the tracks and the waypoints the file holds (also while asking about a
    /// duplicate).
    var routeCount = 0
    var trackCount = 0
    var waypointCount = 0

    /// "2 routes and 12 waypoints from Bannerman Pass.gpx".
    var headline: String {
        if let error { return error }
        if isLoading { return "Reading \(fileName)…" }
        var parts: [String] = []
        if routeCount > 0 { parts.append("\(routeCount) route\(routeCount == 1 ? "" : "s")") }
        if trackCount > 0 { parts.append("\(trackCount) track\(trackCount == 1 ? "" : "s")") }
        if waypointCount > 0 { parts.append("\(waypointCount) waypoint\(waypointCount == 1 ? "" : "s")") }
        let list: String
        switch parts.count {
        case 0: list = "Nothing"
        case 1: list = parts[0]
        default: list = parts.dropLast().joined(separator: ", ") + " and " + (parts.last ?? "")
        }
        return "\(list) from \(fileName)"
    }
}

enum GPXError: Error, LocalizedError, Equatable {
    case unreadable(String)
    case empty
    case notImplemented

    var errorDescription: String? {
        switch self {
        case .unreadable(let why): return "This GPX file couldn't be read: \(why)"
        case .empty: return "This GPX file has no waypoints, routes or tracks."
        case .notImplemented: return "GPX import isn't available yet."
        }
    }
}

/// One GPX file imported: to recognise it when it comes again ("Replace / Keep Both").
struct GPXImportRecord: Codable, Hashable, Sendable {
    var fileName: String
    /// FNV-1a 64 of the file's bytes.
    var contentHash: String
    var importedAt: Date
    var waypointIDs: [UUID]
    var routeIDs: [UUID]
}

/// A GPX file of some items, made when it is shared (the share sheet, AirDrop, Files).
struct GPXExportFile: Transferable, Sendable {
    var name: String
    var waypoints: [Waypoint] = []
    var routes: [Route] = []
    var notes: [MapNote] = []

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(exportedContentType: .gpx) { file in
            SentTransferredFile(try file.write())
        }
    }

    /// Writes the file into the temporary GPX folder ("<name>.gpx").
    func write() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("GPX", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let url = folder.appendingPathComponent(TrackRecorder.fileSafeName(name)).appendingPathExtension("gpx")
        let text = GPXService.export(name: name, waypoints: waypoints, routes: routes, notes: notes)
        try Data(text.utf8).write(to: url, options: .atomic)
        return url
    }
}

/// GPX 1.0/1.1 import (Files, "Open in", AirDrop, the share sheet) and export (docs/design/
/// explorer.md, GPX). Use from the main thread; parsing runs in the background.
@MainActor
@Observable
final class GPXService {
    /// The last import, shown as a summary sheet ("2 routes, 12 waypoints from …").
    var lastImport: GPXImportSummary?

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored private var importTask: Task<Void, Never>?
    /// The file waiting for Replace / Keep Both.
    @ObservationIgnored private var pending: (document: GPXDocument, hash: String, previous: GPXImportRecord)?
    @ObservationIgnored private var records: [GPXImportRecord] = []
    @ObservationIgnored private var recordsLoaded = false
    @ObservationIgnored private var presentedSummaryIDs: Set<UUID> = []
    @ObservationIgnored private let writer = GeoItemWriter()

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Imports a .gpx file: its waypoints, routes and tracks go into the geo item store (a file
    /// imported before is offered to replace or keep both). Handles security-scoped URLs and
    /// removes the Inbox copy "Open in" leaves.
    func importFile(at url: URL) {
        let fileName = url.lastPathComponent
        importTask?.cancel()
        pending = nil
        let summaryID = UUID()
        lastImport = GPXImportSummary(id: summaryID, fileName: fileName, isLoading: true)
        importTask = Task { [weak self] in
            let loaded = await Task.detached(priority: .userInitiated) { () -> Result<(GPXDocument, String), GPXError> in
                let scoped = url.startAccessingSecurityScopedResource()
                defer {
                    if scoped { url.stopAccessingSecurityScopedResource() }
                    GPXService.removeInboxCopy(url)
                }
                let data: Data
                do {
                    data = try Data(contentsOf: url)
                } catch {
                    return .failure(.unreadable(error.localizedDescription))
                }
                do {
                    let document = try GPXService.parse(data, fileName: fileName)
                    return .success((document, GPXService.contentHash(data)))
                } catch let error as GPXError {
                    return .failure(error)
                } catch {
                    return .failure(.unreadable(error.localizedDescription))
                }
            }.value
            guard let self, !Task.isCancelled, self.lastImport?.id == summaryID else { return }
            switch loaded {
            case .failure(let error):
                self.lastImport = GPXImportSummary(id: summaryID, fileName: fileName, error: error.errorDescription)
            case .success(let (document, hash)):
                self.received(document, hash: hash, fileName: fileName, summaryID: summaryID)
            }
        }
    }

    /// The duplicate question's answer: replace the items of the earlier import, or keep both.
    func resolveDuplicate(replace: Bool) {
        guard let pending, let summary = lastImport else { return }
        self.pending = nil
        commit(pending.document, hash: pending.hash, fileName: summary.fileName, summaryID: summary.id,
               replacing: replace ? pending.previous : nil)
    }

    /// The summary to present now, once (whichever screen asks first presents it).
    func takeSummaryToPresent() -> GPXImportSummary? {
        guard let summary = lastImport, !presentedSummaryIDs.contains(summary.id) else { return nil }
        presentedSummaryIDs.insert(summary.id)
        return summary
    }

    /// The summary sheet was closed: an unanswered duplicate question keeps nothing.
    func dismissSummary(_ id: UUID) {
        guard lastImport?.id == id else { return }
        pending = nil
        if lastImport?.duplicateImportedAt != nil || lastImport?.isLoading == true {
            importTask?.cancel()
        }
        lastImport = nil
    }

    // MARK: - Import internals

    private func received(_ document: GPXDocument, hash: String, fileName: String, summaryID: UUID) {
        loadRecordsIfNeeded()
        if let previous = records.last(where: { $0.fileName == fileName && $0.contentHash == hash }),
           let store = model?.geoItems,
           previous.routeIDs.contains(where: { store.route(id: $0) != nil })
            || previous.waypointIDs.contains(where: { store.waypoint(id: $0) != nil }) {
            pending = (document: document, hash: hash, previous: previous)
            var summary = Self.counts(of: document, fileName: fileName, id: summaryID)
            summary.duplicateImportedAt = previous.importedAt
            lastImport = summary
            return
        }
        commit(document, hash: hash, fileName: fileName, summaryID: summaryID, replacing: nil)
    }

    private func commit(_ document: GPXDocument, hash: String, fileName: String, summaryID: UUID,
                        replacing previous: GPXImportRecord?) {
        guard let store = model?.geoItems else { return }
        if let previous {
            for id in previous.routeIDs { store.deleteRoute(id) }
            for id in previous.waypointIDs { store.deleteWaypoint(id) }
            records.removeAll { $0 == previous }
        }
        store.save(document.waypoints)
        // Oldest first, so the file's first route ends up on top (the store lists newest first).
        for route in document.routes.reversed() { store.save(route) }
        let record = GPXImportRecord(fileName: fileName, contentHash: hash, importedAt: Date(),
                                     waypointIDs: document.waypoints.map(\.id), routeIDs: document.routes.map(\.id))
        records.append(record)
        saveRecords()
        var summary = Self.counts(of: document, fileName: fileName, id: summaryID)
        summary.waypointIDs = record.waypointIDs
        summary.routeIDs = record.routeIDs
        lastImport = summary
        // Statistics for the new routes, in the background.
        if let terrain = model?.terrain {
            let routes = document.routes
            Task { [weak self] in
                for route in routes {
                    guard let result = await RoutePlanner.bestProfile(for: route, terrain: terrain) else { continue }
                    guard let store = self?.model?.geoItems, var current = store.route(id: route.id),
                          current.geometryHash == route.geometryHash else { continue }
                    current.stats = RoutePlanner.stats(for: current, profile: result.profile)
                    store.save(current)
                }
            }
        }
    }

    private static func counts(of document: GPXDocument, fileName: String, id: UUID) -> GPXImportSummary {
        var summary = GPXImportSummary(id: id, fileName: fileName)
        summary.routeCount = document.routes.filter { $0.kind != .gpxTrack }.count
        summary.trackCount = document.routes.filter { $0.kind == .gpxTrack }.count
        summary.waypointCount = document.waypoints.count
        var all = document.waypoints.map(\.position)
        for route in document.routes { all += route.latLons }
        summary.bounds = GeoBounds.enclosing(all)
        return summary
    }

    private var recordsURL: URL? {
        model?.geoItems.directory.appendingPathComponent("gpx-imports.json")
    }

    private struct RecordsFile: Codable {
        var version: Int
        var imports: [GPXImportRecord]
    }

    private func loadRecordsIfNeeded() {
        guard !recordsLoaded, let url = recordsURL else { return }
        recordsLoaded = true
        guard let data = try? Data(contentsOf: url) else { return }
        records = (try? JSONDecoder().decode(RecordsFile.self, from: data))?.imports ?? []
    }

    private func saveRecords() {
        guard let url = recordsURL,
              let data = try? JSONEncoder().encode(RecordsFile(version: 1, imports: records)) else { return }
        writer.write(data, to: url)
    }

    /// "Open in" leaves a copy in Documents/Inbox: it is read now, so it goes.
    nonisolated static func removeInboxCopy(_ url: URL) {
        let parent = url.deletingLastPathComponent()
        guard parent.lastPathComponent == "Inbox", parent.deletingLastPathComponent().lastPathComponent == "Documents"
        else { return }
        try? FileManager.default.removeItem(at: url)
    }

    /// FNV-1a 64 of the bytes, hex.
    nonisolated static func contentHash(_ data: Data) -> String {
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
            for byte in buffer {
                h ^= UInt64(byte)
                h = h &* 0x0000_0100_0000_01b3
            }
        }
        return String(h, radix: 16)
    }

    // MARK: - Parse

    /// Parses GPX 1.0/1.1 (`wpt`, `rte`/`rtept`, `trk`/`trkseg`/`trkpt`, `ele`, `name`, `desc`).
    nonisolated static func parse(_ data: Data, fileName: String) throws -> GPXDocument {
        let parser = XMLParser(data: data)
        parser.shouldProcessNamespaces = true
        parser.shouldResolveExternalEntities = false
        let delegate = GPXParserDelegate(fileName: fileName)
        parser.delegate = delegate
        let ok = parser.parse()
        if let why = delegate.failure {
            throw GPXError.unreadable(why)
        }
        guard ok else {
            let reason = parser.parserError?.localizedDescription ?? "it isn't valid XML"
            throw GPXError.unreadable("line \(parser.lineNumber): \(reason)")
        }
        guard delegate.sawRoot else { throw GPXError.unreadable("it isn't a GPX file") }
        let document = delegate.finish()
        guard !document.isEmpty else { throw GPXError.empty }
        return document
    }

    // MARK: - Export

    /// A GPX 1.1 document of these items (routes as `<rte>`, imported tracks as `<trk>`, notes as
    /// `<wpt>` with their text), for the share sheet. A planned route's traced points carry
    /// `<type>trace</type>` so MapMe reads them back as trace, not handles.
    nonisolated static func export(name: String, waypoints: [Waypoint], routes: [Route], notes: [MapNote] = []) -> String {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        let esc = Track.xmlEscaped
        var out = """
        <?xml version="1.0" encoding="UTF-8"?>
        <gpx version="1.1" creator="MapMe" xmlns="http://www.topografix.com/GPX/1/1" \
        xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" \
        xsi:schemaLocation="http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd">
          <metadata>
            <name>\(esc(name))</name>
            <time>\(iso.string(from: Date()))</time>
          </metadata>

        """
        func coordinates(_ lat: Double, _ lon: Double) -> String {
            "lat=\"\(String(format: "%.7f", lat))\" lon=\"\(String(format: "%.7f", lon))\""
        }
        func elevation(_ e: Double?) -> String {
            guard let e, e.isFinite else { return "" }
            return "<ele>\(String(format: "%.1f", e))</ele>"
        }
        for w in waypoints {
            out += "  <wpt \(coordinates(w.position.lat, w.position.lon))>\(elevation(w.elevationM))"
            out += "<time>\(iso.string(from: w.createdAt))</time><name>\(esc(w.name))</name>"
            if let c = w.comment, !c.isEmpty { out += "<desc>\(esc(c))</desc>" }
            out += "<sym>\(esc(GPXSymbols.gpxName(for: w.symbol)))</sym></wpt>\n"
        }
        for n in notes {
            out += "  <wpt \(coordinates(n.position.lat, n.position.lon))>\(elevation(n.elevationM))"
            out += "<time>\(iso.string(from: n.createdAt))</time><name>\(esc(n.title))</name>"
            if !n.text.isEmpty { out += "<desc>\(esc(n.text))</desc>" }
            out += "<sym>Note</sym><type>Note</type></wpt>\n"
        }
        for r in routes {
            if r.kind == .gpxTrack {
                out += "  <trk>\n    <name>\(esc(r.name))</name>\n"
                if let c = r.comment, !c.isEmpty { out += "    <desc>\(esc(c))</desc>\n" }
                out += "    <trkseg>\n"
                for p in r.points {
                    out += "      <trkpt \(coordinates(p.lat, p.lon))>\(elevation(p.ele))</trkpt>\n"
                }
                out += "    </trkseg>\n  </trk>\n"
            } else {
                out += "  <rte>\n    <name>\(esc(r.name))</name>\n"
                if let c = r.comment, !c.isEmpty { out += "    <desc>\(esc(c))</desc>\n" }
                for p in r.points {
                    out += "    <rtept \(coordinates(p.lat, p.lon))>\(elevation(p.ele))"
                    if let n = p.name, !n.isEmpty { out += "<name>\(esc(n))</name>" }
                    if !p.isAnchor { out += "<type>trace</type>" }
                    out += "</rtept>\n"
                }
                out += "  </rte>\n"
            }
        }
        out += "</gpx>\n"
        return out
    }
}

/// GPX `<sym>` names (Garmin's, which Gaia GPS, AllTrails and most apps read) and MapMe's symbols.
enum GPXSymbols {
    static func gpxName(for symbol: WaypointSymbol) -> String {
        switch symbol {
        case .flag: return "Flag, Blue"
        case .pin: return "Pin, Blue"
        case .camp: return "Campground"
        case .water: return "Drinking Water"
        case .peak: return "Summit"
        case .hut: return "Lodging"
        case .parking: return "Parking Area"
        case .viewpoint: return "Scenic Area"
        case .danger: return "Danger Area"
        case .start: return "Trail Head"
        case .finish: return "Flag, Red"
        }
    }

    /// The symbol for a file's `<sym>` (and `<type>`): exact names first, then keywords
    /// ("campsite", "spring", "summit", "shelter", "trailhead", …); a flag otherwise.
    static func symbol(sym: String?, type: String? = nil) -> WaypointSymbol {
        let words = [sym, type].compactMap { $0?.lowercased().trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        for w in words {
            if let exact = WaypointSymbol.allCases.first(where: { gpxName(for: $0).lowercased() == w || $0.rawValue == w }) {
                return exact
            }
        }
        let keywords: [(WaypointSymbol, [String])] = [
            (.camp, ["camp", "tent", "bivouac", "bivy"]),
            (.water, ["water", "spring", "drink", "stream", "river", "lake", "fountain", "tap"]),
            (.peak, ["summit", "peak", "mountain", "top", "hill"]),
            (.hut, ["hut", "shelter", "lodg", "cabin", "hotel", "building", "residence", "house", "refuge", "cave"]),
            (.parking, ["parking", "car", "lot"]),
            (.viewpoint, ["view", "scenic", "vista", "overlook", "photo", "camera", "lookout"]),
            (.danger, ["danger", "hazard", "caution", "warning", "skull", "crossing"]),
            (.start, ["trailhead", "trail head", "start"]),
            (.finish, ["finish", "end", "checkered", "goal"]),
            (.pin, ["pin"]),
        ]
        for w in words {
            for (symbol, keys) in keywords where keys.contains(where: { w.contains($0) }) {
                return symbol
            }
        }
        return .flag
    }
}

/// `XMLParser` delegate building a `GPXDocument`. One per parse, on the parsing thread.
private final class GPXParserDelegate: NSObject, XMLParserDelegate {
    /// One `wpt`, `rtept` or `trkpt` being read.
    private struct PointBuilder {
        var lat: Double
        var lon: Double
        var ele: Double?
        var time: Date?
        var name: String?
        var desc: String?
        var cmt: String?
        var sym: String?
        var type: String?
    }

    /// A `rte` or `trk` being read.
    private struct LineBuilder {
        var name: String?
        var desc: String?
        var points: [RoutePoint] = []
    }

    let fileName: String
    private(set) var sawRoot = false
    /// Set when the file is XML but not something to read.
    private(set) var failure: String?

    private var document = GPXDocument()
    private var path: [String] = []
    private var text = ""
    private var point: PointBuilder?
    private var line: LineBuilder?
    private var extensionsDepth = 0
    private let iso = ISO8601DateFormatter()
    private let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    init(fileName: String) {
        self.fileName = fileName
    }

    private var baseName: String { (fileName as NSString).deletingPathExtension }

    /// The document with every item named and stamped as imported from this file.
    func finish() -> GPXDocument {
        var doc = document
        let origin = GeoItemOrigin.gpx(fileName: fileName)
        for i in doc.waypoints.indices { doc.waypoints[i].origin = origin }
        for i in doc.routes.indices { doc.routes[i].origin = origin }
        return doc
    }

    func parser(_ parser: XMLParser, didStartElement elementName: String, namespaceURI: String?,
                qualifiedName qName: String?, attributes attributeDict: [String: String] = [:]) {
        let name = Self.localName(elementName)
        if path.isEmpty {
            guard name == "gpx" else {
                failure = "it isn't a GPX file"
                parser.abortParsing()
                return
            }
            sawRoot = true
            document.creator = attributeDict["creator"]
        }
        path.append(name)
        text = ""
        if extensionsDepth > 0 || name == "extensions" {
            extensionsDepth += 1
            return
        }
        switch name {
        case "wpt", "rtept", "trkpt":
            if let lat = attributeDict["lat"].flatMap(Self.number), let lon = attributeDict["lon"].flatMap(Self.number),
               (-90...90).contains(lat), (-180...180).contains(lon) {
                point = PointBuilder(lat: lat, lon: lon)
            } else {
                point = nil
                document.skippedPoints += 1
            }
        case "rte", "trk":
            line = LineBuilder()
        default:
            break
        }
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) {
        guard extensionsDepth == 0 else { return }
        text += string
    }

    func parser(_ parser: XMLParser, foundCDATA CDATABlock: Data) {
        guard extensionsDepth == 0 else { return }
        text += String(decoding: CDATABlock, as: UTF8.self)
    }

    func parser(_ parser: XMLParser, didEndElement elementName: String, namespaceURI: String?, qualifiedName qName: String?) {
        let name = Self.localName(elementName)
        defer {
            if !path.isEmpty { path.removeLast() }
            text = ""
        }
        if extensionsDepth > 0 {
            extensionsDepth -= 1
            return
        }
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let parent = path.count >= 2 ? path[path.count - 2] : ""
        switch name {
        case "ele":
            if let e = Self.number(value), e.isFinite { point?.ele = e }
        case "time":
            if point != nil { point?.time = iso.date(from: value) ?? isoFractional.date(from: value) }
        case "name":
            if parent == "wpt" || parent == "rtept" || parent == "trkpt" {
                point?.name = value.isEmpty ? nil : value
            } else if parent == "rte" || parent == "trk" {
                line?.name = value.isEmpty ? nil : value
            } else if parent == "metadata" || parent == "gpx" {
                document.name = value.isEmpty ? nil : value
            }
        case "desc", "cmt":
            let target = value.isEmpty ? nil : value
            if parent == "wpt" || parent == "rtept" || parent == "trkpt" {
                if name == "desc" { point?.desc = target } else { point?.cmt = target }
            } else if parent == "rte" || parent == "trk" {
                if line?.desc == nil || name == "desc" { line?.desc = target }
            }
        case "sym":
            point?.sym = value.isEmpty ? nil : value
        case "type":
            if parent == "wpt" || parent == "rtept" || parent == "trkpt" { point?.type = value.isEmpty ? nil : value }
        case "wpt":
            if let p = point {
                let n = document.waypoints.count + 1
                var w = Waypoint(name: p.name ?? "Waypoint \(n)", position: LatLon(lat: p.lat, lon: p.lon))
                w.elevationM = p.ele
                w.symbol = GPXSymbols.symbol(sym: p.sym, type: p.type)
                // Garmin units repeat the name in <desc> and keep the comment in <cmt>.
                let desc = p.desc == p.name ? nil : p.desc
                w.comment = desc ?? p.cmt
                if let t = p.time { w.createdAt = t }
                document.waypoints.append(w)
            }
            point = nil
        case "rtept", "trkpt":
            if let p = point {
                // A route's points are its handles, except what MapMe exported as trace.
                let anchor = name == "rtept" && p.type?.lowercased() != "trace"
                line?.points.append(RoutePoint(lat: p.lat, lon: p.lon, ele: p.ele, isAnchor: anchor,
                                               name: name == "rtept" ? p.name : nil))
            }
            point = nil
        case "rte", "trk":
            if let l = line, !l.points.isEmpty {
                let isTrack = name == "trk"
                let fallback = document.name ?? baseName
                let index = document.routes.count + 1
                var route = Route(name: l.name ?? (index == 1 ? fallback : "\(fallback) \(index)"),
                                  kind: isTrack ? .gpxTrack : .gpxRoute, points: l.points)
                route.comment = l.desc
                document.routes.append(route)
            }
            line = nil
        default:
            break
        }
    }

    /// "gpx:wpt" → "wpt" (namespace processing normally does this already).
    private static func localName(_ name: String) -> String {
        guard let colon = name.lastIndex(of: ":") else { return name }
        return String(name[name.index(after: colon)...])
    }

    private static func number(_ s: String) -> Double? {
        Double(s.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}
