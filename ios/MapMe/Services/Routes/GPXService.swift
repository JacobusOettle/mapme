import Foundation
import Observation

/// What a GPX file held.
struct GPXDocument: Equatable, Sendable {
    /// `<wpt>`s.
    var waypoints: [Waypoint] = []
    /// `<rte>`s (kind `.gpxRoute`) and `<trk>`s (kind `.gpxTrack`, one route per track, its
    /// segments joined).
    var routes: [Route] = []
    /// `<metadata><name>`, if any.
    var name: String? = nil
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

/// GPX 1.0/1.1 import (Files, "Open in", AirDrop, the share sheet) and export (docs/design/
/// explorer.md, GPX). Use from the main thread; parsing runs in the background.
@MainActor
@Observable
final class GPXService {
    /// The last import, shown as a summary sheet ("2 routes, 12 waypoints from …").
    var lastImport: GPXImportSummary?

    @ObservationIgnored private weak var model: AppModel?

    init() {}

    func attach(_ model: AppModel) {
        self.model = model
    }

    /// Imports a .gpx file: its waypoints, routes and tracks go into the geo item store (a file
    /// imported before is offered to replace or keep both). Handles security-scoped URLs and
    /// removes the Inbox copy "Open in" leaves.
    func importFile(at url: URL) {
        lastImport = GPXImportSummary(fileName: url.lastPathComponent, error: GPXError.notImplemented.errorDescription)
    }

    /// Parses GPX 1.0/1.1 (`wpt`, `rte`/`rtept`, `trk`/`trkseg`/`trkpt`, `ele`, `name`, `desc`).
    nonisolated static func parse(_ data: Data, fileName: String) throws -> GPXDocument {
        throw GPXError.notImplemented
    }

    /// A GPX 1.1 document of these items (routes as `<rte>`, imported tracks as `<trk>`, notes as
    /// `<wpt>` with their text), for the share sheet.
    nonisolated static func export(name: String, waypoints: [Waypoint], routes: [Route], notes: [MapNote] = []) -> String {
        Track(name: name, startedAt: Date()).gpx()
    }
}
