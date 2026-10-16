import CoreGraphics
import CoreLocation
import Observation
import SwiftUI
import UIKit

/// A route being planned or edited: its points, undo history, the selected handle, the freehand
/// trace in progress and the live statistics (docs/design/explorer.md, Routes: Plan).
@MainActor
@Observable
final class RouteEditSession {
    /// The route edited (nil: a new one).
    let routeID: UUID?
    private(set) var points: [RoutePoint]
    /// The handle tapped (an index into `points`), whose actions the bar shows.
    var selectedAnchor: Int?
    /// One-finger drags trace points along a trail; two fingers move the map.
    var freehand = false
    /// The stroke being traced, in map pixels (drawn live, added on lift).
    private(set) var liveTrace: [CGPoint] = []
    /// The statistics of `points` (nil while the first ones are worked out).
    private(set) var profile: RouteProfile?
    /// `Route.geometryHash` of the points `profile` is for.
    private(set) var profileHash: Int?
    private(set) var isComputing = false
    /// Bumped on every change of the points.
    private(set) var revision = 0

    @ObservationIgnored private var undoStack: [[RoutePoint]] = []
    @ObservationIgnored private var statsTask: Task<Void, Never>?
    @ObservationIgnored private weak var terrain: TerrainProvider?
    /// Statistics wait this long after the last change (a drag changes the points every frame).
    static let statsDelay: UInt64 = 300_000_000

    init(routeID: UUID?, points: [RoutePoint], terrain: TerrainProvider?) {
        self.routeID = routeID
        self.points = points
        self.terrain = terrain
    }

    var anchorCount: Int { points.filter(\.isAnchor).count }
    var canUndo: Bool { !undoStack.isEmpty }
    var hasChanges: Bool { !undoStack.isEmpty }
    /// The length right now (before the profile is worked out).
    var length: Double { RouteGeometry.length(points.map(\.latLon)) }

    /// Remembers the points for Undo (before a change).
    func checkpoint() {
        undoStack.append(points)
        if undoStack.count > 100 { undoStack.removeFirst() }
    }

    func undo() {
        guard let previous = undoStack.popLast() else { return }
        points = previous
        selectedAnchor = nil
        changed()
    }

    /// Adds a handle at the end.
    func appendAnchor(_ p: LatLon, name: String? = nil) {
        checkpoint()
        points.append(RoutePoint(p, isAnchor: true, name: name))
        selectedAnchor = nil
        changed()
    }

    /// Inserts a handle between points `segment` and `segment + 1`.
    func insertAnchor(_ p: LatLon, afterIndex segment: Int) {
        guard segment >= 0, segment < points.count - 1 else { return appendAnchor(p) }
        checkpoint()
        points.insert(RoutePoint(p, isAnchor: true), at: segment + 1)
        selectedAnchor = nil
        changed()
    }

    /// Moves point `index` (a drag: `checkpoint()` once when it starts).
    func move(_ index: Int, to p: LatLon) {
        guard points.indices.contains(index) else { return }
        points[index].lat = p.lat
        points[index].lon = p.lon
        changed()
    }

    /// Deletes a handle and the trace between it and the handles either side (the line then goes
    /// straight from one neighbour to the other).
    func deleteAnchor(_ index: Int) {
        guard points.indices.contains(index) else { return }
        checkpoint()
        let previous = points[..<index].lastIndex(where: \.isAnchor) ?? -1
        let next = points[(index + 1)...].firstIndex(where: \.isAnchor) ?? points.count
        points.removeSubrange((previous + 1)..<next)
        selectedAnchor = nil
        changed()
    }

    /// The handle becomes the route's end: everything after it goes.
    func makeEnd(_ index: Int) {
        guard points.indices.contains(index), index < points.count - 1 else { return }
        checkpoint()
        points.removeSubrange((index + 1)...)
        selectedAnchor = nil
        changed()
    }

    func rename(_ index: Int, to name: String) {
        guard points.indices.contains(index) else { return }
        checkpoint()
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        points[index].name = trimmed.isEmpty ? nil : trimmed
        changed()
    }

    // MARK: Freehand

    func beginTrace(at pixel: CGPoint) {
        checkpoint()
        liveTrace = [pixel]
        selectedAnchor = nil
    }

    func extendTrace(to pixel: CGPoint) {
        liveTrace.append(pixel)
    }

    /// Ends the trace: its points (simplified) join the route, the last one a handle; the first
    /// one too when the route was empty.
    func endTrace(cancelled: Bool, toLatLon: (CGPoint) -> LatLon?, tolerance: CGFloat) {
        let trace = liveTrace
        liveTrace = []
        guard !cancelled, trace.count >= 2 else {
            // Nothing added: the checkpoint is not a step to undo.
            if !undoStack.isEmpty { undoStack.removeLast() }
            changed()
            return
        }
        let keep = RouteGeometry.simplify(trace, tolerance: tolerance)
        let coordinates = keep.compactMap { toLatLon(trace[$0]) }
        guard coordinates.count >= 2 else {
            if !undoStack.isEmpty { undoStack.removeLast() }
            changed()
            return
        }
        for (i, p) in coordinates.enumerated() {
            let isFirst = i == 0, isLast = i == coordinates.count - 1
            points.append(RoutePoint(p, isAnchor: isLast || (isFirst && points.isEmpty)))
        }
        changed()
    }

    // MARK: Statistics

    private func changed() {
        revision += 1
        scheduleStats()
    }

    /// Works the statistics out 0.3 s after the last change, off the main thread.
    func scheduleStats() {
        statsTask?.cancel()
        let snapshot = Route(name: "", kind: .planned, points: points)
        guard snapshot.points.count >= 2, let terrain else {
            profile = nil
            profileHash = nil
            isComputing = false
            return
        }
        isComputing = true
        statsTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: Self.statsDelay)
            guard !Task.isCancelled else { return }
            let result = await RoutePlanner.bestProfile(for: snapshot, terrain: terrain)
            guard !Task.isCancelled, let self else { return }
            self.profile = result
            self.profileHash = snapshot.geometryHash
            self.isComputing = false
        }
    }

    func cancelStats() {
        statsTask?.cancel()
        statsTask = nil
    }
}

/// Planning and editing a route on the map (`MapScreenContext.tool`): taps add handles (or
/// insert one on the line), handles drag (the map stays still), a tapped handle offers Delete,
/// Name… and Make End; in freehand mode one finger traces along a trail.
@MainActor
final class RouteEditTool: MapTool {
    /// Handles are hit within this (points): a 44 pt target.
    static let handleRadius: CGFloat = 22
    /// A tap this close to the line inserts a handle there.
    static let lineRadius: CGFloat = 22
    /// The freehand trace keeps a point every 6 pt…
    static let traceSpacing: CGFloat = 6
    /// …and simplifies to within 1 pt when the finger lifts.
    static let traceTolerance: CGFloat = 1

    let session: RouteEditSession
    private weak var model: AppModel?
    private weak var context: MapScreenContext?
    private var dragging: Int?
    private var tracing = false
    private var lastTraceView: CGPoint?

    private init(session: RouteEditSession, model: AppModel, context: MapScreenContext) {
        self.session = session
        self.model = model
        self.context = context
    }

    /// Starts planning (`route` nil, from `points`) or editing a route on this screen.
    @discardableResult
    static func start(context: MapScreenContext, model: AppModel, route: Route? = nil,
                      points: [RoutePoint] = []) -> RouteEditTool {
        if let current = context.tool as? RouteEditTool { current.finish(save: false) }
        let session = RouteEditSession(routeID: route?.id, points: route?.points ?? points, terrain: model.terrain)
        let tool = RouteEditTool(session: session, model: model, context: context)
        context.clearSelection()
        context.tool = tool
        tool.overlay?.setDraft(session)
        session.scheduleStats()
        return tool
    }

    /// The screen's route overlay (draws the draft; knows the camera).
    private var overlay: RouteOverlay? {
        context?.overlays?.overlays.lazy.compactMap { $0 as? RouteOverlay }.first
    }

    func makeBar() -> AnyView {
        AnyView(RouteEditBar(tool: self, session: session))
    }

    // MARK: Touches

    func tap(at pixel: CGPoint, viewPoint: CGPoint) -> Bool {
        if let i = anchor(near: viewPoint) {
            session.selectedAnchor = session.selectedAnchor == i ? nil : i
            overlay?.draftDidChange()
            return true
        }
        guard let p = overlay?.latLon(forPixel: pixel) else { return true }
        if session.selectedAnchor != nil {
            // A tap elsewhere first just closes the handle's actions.
            session.selectedAnchor = nil
            overlay?.draftDidChange()
            return true
        }
        if let segment = segment(near: viewPoint) {
            session.insertAnchor(p, afterIndex: segment)
        } else {
            session.appendAnchor(p)
        }
        overlay?.draftDidChange()
        return true
    }

    func beginDrag(at pixel: CGPoint, viewPoint: CGPoint) -> Bool {
        if let i = anchor(near: viewPoint) {
            session.checkpoint()
            session.selectedAnchor = nil
            dragging = i
            return true
        }
        guard session.freehand else { return false }
        tracing = true
        lastTraceView = viewPoint
        session.beginTrace(at: pixel)
        overlay?.draftDidChange()
        return true
    }

    func drag(to pixel: CGPoint, viewPoint: CGPoint) {
        if let i = dragging {
            if let p = overlay?.latLon(forPixel: pixel) { session.move(i, to: p) }
            overlay?.draftDidChange()
        } else if tracing {
            if let last = lastTraceView, hypot(viewPoint.x - last.x, viewPoint.y - last.y) < Self.traceSpacing { return }
            lastTraceView = viewPoint
            session.extendTrace(to: pixel)
            overlay?.draftDidChange()
        }
    }

    func endDrag(cancelled: Bool) {
        if dragging != nil {
            dragging = nil
            if cancelled { session.undo() }
        } else if tracing {
            tracing = false
            lastTraceView = nil
            let zoom = overlay?.lastFrame?.camera.zoom ?? 1
            let overlay = self.overlay
            session.endTrace(cancelled: cancelled, toLatLon: { overlay?.latLon(forPixel: $0) },
                             tolerance: Self.traceTolerance / max(zoom, 1e-6))
        }
        overlay?.draftDidChange()
    }

    /// The handle nearest a view point, within reach.
    private func anchor(near v: CGPoint) -> Int? {
        guard let overlay else { return nil }
        var best: (index: Int, d: CGFloat)?
        for (i, p) in session.points.enumerated() where p.isAnchor {
            guard let q = overlay.viewPoint(for: p.latLon) else { continue }
            let d = hypot(q.x - v.x, q.y - v.y)
            if d <= Self.handleRadius, d < (best?.d ?? .infinity) { best = (index: i, d: d) }
        }
        return best?.index
    }

    /// The segment of the line nearest a view point, within reach.
    private func segment(near v: CGPoint) -> Int? {
        guard let overlay, session.points.count >= 2 else { return nil }
        let views = session.points.map { overlay.viewPoint(for: $0.latLon) }
        var best: (index: Int, d: CGFloat)?
        for i in 1..<views.count {
            guard let a = views[i - 1], let b = views[i] else { continue }
            let hit = RouteGeometry.distance(from: v, toSegment: a, b)
            // Past either end of a segment the tap is about a handle (tested before) or extends.
            guard hit.t > 0, hit.t < 1 else { continue }
            if hit.distance <= Self.lineRadius, hit.distance < (best?.d ?? .infinity) { best = (index: i - 1, d: hit.distance) }
        }
        return best?.index
    }

    // MARK: Commands

    func toggleFreehand() {
        session.freehand.toggle()
    }

    /// Adds a handle at your position (an empty route's start).
    func startHere() {
        guard let fix = model?.location.location else { return }
        session.appendAnchor(LatLon(fix.coordinate))
        overlay?.draftDidChange()
    }

    /// Adds a handle at `p` (the dropped pin's or a waypoint's "Route Here").
    func appendAnchor(_ p: LatLon, name: String? = nil) {
        session.appendAnchor(p, name: name)
        overlay?.draftDidChange()
    }

    func undo() {
        session.undo()
        overlay?.draftDidChange()
    }

    func deleteSelected() {
        guard let i = session.selectedAnchor else { return }
        session.deleteAnchor(i)
        overlay?.draftDidChange()
    }

    func makeSelectedEnd() {
        guard let i = session.selectedAnchor else { return }
        session.makeEnd(i)
        overlay?.draftDidChange()
    }

    func renameSelected(_ name: String) {
        guard let i = session.selectedAnchor else { return }
        session.rename(i, to: name)
    }

    /// Done (`save`) or Cancel. Done names a new route "Route <date>" (rename later) and selects it.
    func finish(save: Bool) {
        session.cancelStats()
        defer {
            overlay?.setDraft(nil)
            if context?.tool === self { context?.tool = nil }
        }
        guard save, let model, session.points.count >= 2 else { return }
        var route: Route
        if let id = session.routeID, var existing = model.geoItems.route(id: id) {
            existing.points = session.points
            existing.updatedAt = Date()
            route = existing
        } else {
            route = Route(name: "Route " + Date().formatted(date: .abbreviated, time: .shortened), kind: .planned,
                          points: session.points)
        }
        if let profile = session.profile, session.profileHash == route.geometryHash {
            route.stats = RoutePlanner.stats(for: route, profile: profile.profile)
        } else {
            route.stats = nil
        }
        model.geoItems.save(route)
        context?.selectedItem = GeoItemRef(kind: .route, id: route.id)
    }
}

/// The bottom bar while a route is planned or edited: live statistics, the selected handle's
/// actions, Undo, freehand, Cancel and Done.
@MainActor
struct RouteEditBar: View {
    let tool: RouteEditTool
    let session: RouteEditSession
    @Environment(AppModel.self) private var model
    @State private var naming = false
    @State private var nameText = ""
    @State private var confirmCancel = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            stats
            if let i = session.selectedAnchor, i < session.points.count {
                anchorActions(i)
            } else {
                hint
            }
            controls
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 26, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("routeEditBar")
        .sensoryFeedback(.selection, trigger: session.anchorCount)
        .alert("Name this point", isPresented: $naming) {
            TextField("Name", text: $nameText)
            Button("Cancel", role: .cancel) {}
            Button("Save") { tool.renameSelected(nameText) }
        }
        .confirmationDialog("Discard this route?", isPresented: $confirmCancel, titleVisibility: .visible) {
            Button("Discard Changes", role: .destructive) { tool.finish(save: false) }
            Button("Keep Editing", role: .cancel) {}
        }
    }

    private var stats: some View {
        let units = model.settings.units
        let profile = session.profileHash == Route(name: "", kind: .planned, points: session.points).geometryHash
            ? session.profile : nil
        let distance = profile?.profile.distanceM ?? session.length
        let pace = model.navigator.currentPace()
        let time: TimeInterval? = profile.flatMap { p in
            p.hasElevation ? PaceModel.movingTime(along: p.profile, factor: pace.factor) : nil
        } ?? (session.points.count >= 2 ? PaceModel.flatTime(meters: distance, factor: pace.factor) : nil)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                PanelStat(caption: "Distance", value: GeoFormat.distance(distance, units: units))
                    .accessibilityIdentifier("routeEditDistance")
                PanelStat(caption: "Ascent", value: profile?.hasElevation == true
                    ? "↑" + GeoFormat.elevation(profile?.profile.ascentM ?? 0, units: units) : "—")
                PanelStat(caption: "Descent", value: profile?.hasElevation == true
                    ? "↓" + GeoFormat.elevation(profile?.profile.descentM ?? 0, units: units) : "—")
                PanelStat(caption: "Time", value: time.map { RouteFormat.shortDuration($0) } ?? "—")
            }
            HStack(spacing: 6) {
                if session.isComputing {
                    ProgressView()
                        .controlSize(.mini)
                }
                Text(statsFootnote(profile: profile))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
        }
    }

    private func statsFootnote(profile: RouteProfile?) -> String {
        if session.points.count < 2 { return "Walking time at your pace, moving only" }
        guard let profile else { return session.isComputing ? "Working out the climb…" : "Elevation needs terrain (offline)" }
        if !profile.hasElevation { return "Elevation needs terrain (offline) · time on the flat" }
        return "\(profile.source.caption) · at your pace, moving only"
    }

    private var hint: some View {
        let text: String
        if session.freehand {
            text = "Draw along the trail with one finger. Two fingers move the map."
        } else if session.points.isEmpty {
            text = "Tap the map where the route starts."
        } else {
            text = "Tap to add points, tap the line to insert one. Drag a point to move it."
        }
        return HStack(spacing: 10) {
            Text(text)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            if session.points.isEmpty, model.location.location != nil {
                Button {
                    tool.startHere()
                } label: {
                    Label("Start Here", systemImage: "location.fill")
                        .font(.footnote.weight(.semibold))
                }
                .buttonStyle(.bordered)
                .tint(Theme.route)
                .accessibilityIdentifier("routeStartHereButton")
            }
        }
    }

    private func anchorActions(_ i: Int) -> some View {
        let point = session.points[i]
        let index = session.points[...i].filter(\.isAnchor).count
        return HStack(spacing: 8) {
            Text(point.name ?? "Point \(index)")
                .font(.subheadline.weight(.semibold))
                .lineLimit(1)
            Spacer(minLength: 4)
            Button(role: .destructive) {
                tool.deleteSelected()
            } label: {
                Label("Delete", systemImage: "trash")
            }
            .accessibilityIdentifier("routeDeletePointButton")
            Button {
                nameText = point.name ?? ""
                naming = true
            } label: {
                Label("Name…", systemImage: "character.cursor.ibeam")
            }
            .accessibilityIdentifier("routeNamePointButton")
            if i < session.points.count - 1 {
                Button {
                    tool.makeSelectedEnd()
                } label: {
                    Label("Make End", systemImage: "flag.checkered")
                }
                .accessibilityIdentifier("routeMakeEndButton")
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.bordered)
        .font(.subheadline.weight(.semibold))
    }

    private var controls: some View {
        HStack(spacing: 10) {
            Button {
                tool.undo()
            } label: {
                Image(systemName: "arrow.uturn.backward")
                    .frame(width: 44, height: 44)
            }
            .disabled(!session.canUndo)
            .accessibilityLabel("Undo")
            .accessibilityIdentifier("routeUndoButton")

            Button {
                tool.toggleFreehand()
            } label: {
                Image(systemName: session.freehand ? "pencil.tip.crop.circle.fill" : "pencil.tip.crop.circle")
                    .foregroundStyle(session.freehand ? Theme.route : Color.primary)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel(session.freehand ? "Freehand on" : "Freehand off")
            .accessibilityIdentifier("routeFreehandButton")
            .sensoryFeedback(.selection, trigger: session.freehand)

            Spacer(minLength: 4)

            Button("Cancel") {
                if session.hasChanges { confirmCancel = true } else { tool.finish(save: false) }
            }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("routeCancelButton")

            Button {
                tool.finish(save: true)
            } label: {
                Text("Done")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 6)
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.route)
            .disabled(session.points.count < 2)
            .accessibilityIdentifier("routeDoneButton")
        }
        .font(.body.weight(.semibold))
        .frame(minHeight: 44)
    }
}
