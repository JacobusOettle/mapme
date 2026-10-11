import Charts
import SwiftUI

/// Settings section listing saved tracks; rows open a detail page with GPX export.
struct TracksSection: View {
    @Environment(AppModel.self) private var model
    @State private var summaries: [UUID: SettingsTrackSummary] = [:]
    @State private var confirmClearTrail = false

    init() {}

    var body: some View {
        let tracks = model.tracks.tracks
        let units = model.settings.units
        Section {
            if tracks.isEmpty {
                HStack(spacing: 12) {
                    SymbolBadge(symbol: "point.topleft.down.curvedto.point.bottomright.up", color: Theme.track, size: 36)
                    Text("No saved tracks yet. Start a recording on the map and it shows up here, ready to export as GPX.")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.vertical, 4)
            } else {
                ForEach(tracks) { track in
                    NavigationLink {
                        SavedTrackDetailView(track: track)
                    } label: {
                        SettingsTrackRow(track: track, summary: summaries[track.id], units: units)
                    }
                }
                .onDelete { offsets in
                    deleteTracks(at: offsets, in: tracks)
                }
            }
            Button {
                confirmClearTrail = true
            } label: {
                Label("Clear trail on map", systemImage: "eraser.line.dashed")
            }
            .confirmationDialog("Clear the trail on the map?", isPresented: $confirmClearTrail, titleVisibility: .visible) {
                Button("Clear trail", role: .destructive) { model.tracks.clearCurrent() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("The dotted line of where you walked is removed. Saved tracks are kept.")
            }
            .task(id: tracks.map(\.id)) {
                await summarize(tracks)
            }
        } header: {
            Text("Tracks")
        } footer: {
            if !tracks.isEmpty {
                Text("Swipe left on a track to delete it.")
            }
        }
    }

    @MainActor
    private func summarize(_ tracks: [Track]) async {
        let missing = tracks.filter { summaries[$0.id] == nil }
        guard !missing.isEmpty else { return }
        let computed = await Task.detached(priority: .utility) { () -> [(UUID, SettingsTrackSummary)] in
            missing.map { ($0.id, SettingsTrackSummary($0)) }
        }.value
        for (id, summary) in computed {
            summaries[id] = summary
        }
    }

    @MainActor
    private func deleteTracks(at offsets: IndexSet, in tracks: [Track]) {
        let doomed = offsets.compactMap { $0 < tracks.count ? tracks[$0] : nil }
        withAnimation {
            for track in doomed {
                model.tracks.delete(track)
            }
        }
    }
}

/// Distance, time and climb of a track (computed off the main thread: O(points)).
private struct SettingsTrackSummary: Hashable {
    var distance: Double
    var duration: TimeInterval
    var ascent: Double
    var minElevation: Double?
    var maxElevation: Double?
    var pointCount: Int

    init(_ track: Track) {
        distance = track.distance
        duration = track.duration
        ascent = track.ascent
        pointCount = track.points.count
        var lo = Double.infinity
        var hi = -Double.infinity
        for p in track.points {
            guard let a = p.altitude, a.isFinite else { continue }
            lo = min(lo, a)
            hi = max(hi, a)
        }
        minElevation = lo.isFinite ? lo : nil
        maxElevation = hi.isFinite ? hi : nil
    }

    var averageSpeed: Double? {
        guard duration > 1, distance > 0 else { return nil }
        return distance / duration
    }
}

private struct SettingsTrackRow: View {
    let track: Track
    let summary: SettingsTrackSummary?
    let units: DistanceUnits

    var body: some View {
        HStack(spacing: 12) {
            SymbolBadge(symbol: "figure.hiking", color: Theme.track, size: 36)
            VStack(alignment: .leading, spacing: 3) {
                Text(track.name)
                    .font(.body.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                Text(track.startedAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                if let summary {
                    HStack(spacing: 10) {
                        stat("arrow.left.and.right", GeoFormat.distance(summary.distance, units: units))
                        stat("clock", GeoFormat.duration(summary.duration))
                        stat("arrow.up.right", GeoFormat.elevation(summary.ascent, units: units))
                    }
                } else {
                    Text("\(track.points.count) points")
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func stat(_ symbol: String, _ value: String) -> some View {
        HStack(spacing: 3) {
            Image(systemName: symbol)
                .font(.caption2.weight(.bold))
                .foregroundStyle(Theme.track)
            Text(value)
                .font(.caption.weight(.medium))
                .monospacedDigit()
                .foregroundStyle(Theme.textPrimary)
        }
    }
}

// MARK: - Detail

/// One saved track: route sketch, stats, elevation profile, GPX export.
struct SavedTrackDetailView: View {
    let track: Track
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var details: SettingsTrackDetails? = nil
    @State private var exportURL: URL? = nil
    @State private var exportFailed = false
    @State private var showDelete = false

    init(track: Track) {
        self.track = track
    }

    var body: some View {
        let units = model.settings.units
        ScrollView {
            VStack(spacing: 16) {
                routeCard
                if let details {
                    statsGrid(details.summary, units: units)
                    if details.profile.count >= 2 {
                        profileCard(details, units: units)
                    }
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, minHeight: 120)
                }
                Button(role: .destructive) {
                    showDelete = true
                } label: {
                    Label("Delete track", systemImage: "trash")
                        .font(.body.weight(.semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                        .foregroundStyle(Theme.bad)
                        .background(Theme.bad.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }
                .buttonStyle(.plain)
                .padding(.top, 8)
            }
            .padding(16)
        }
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle(track.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                if let exportURL {
                    ShareLink(item: exportURL, subject: Text(track.name)) {
                        Label("Export GPX", systemImage: "square.and.arrow.up")
                    }
                    .accessibilityIdentifier("exportGPX")
                } else if exportFailed {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundStyle(Theme.warn)
                } else {
                    ProgressView()
                }
            }
        }
        .confirmationDialog("Delete this track?", isPresented: $showDelete, titleVisibility: .visible) {
            Button("Delete \u{201C}\(track.name)\u{201D}", role: .destructive) {
                model.tracks.delete(track)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        }
        .task {
            await prepare()
        }
    }

    @MainActor
    private func prepare() async {
        if exportURL == nil {
            do {
                exportURL = try model.tracks.gpxFile(for: track)
            } catch {
                exportFailed = true
            }
        }
        if details == nil {
            let t = track
            details = await Task.detached(priority: .userInitiated) { SettingsTrackDetails(t) }.value
        }
    }

    private var routeCard: some View {
        ZStack(alignment: .bottomLeading) {
            RoundedRectangle(cornerRadius: Theme.corner, style: .continuous)
                .fill(LinearGradient(colors: [Theme.surfaceRaised, Theme.surface],
                                     startPoint: .top, endPoint: .bottom))
            if let details, details.shape.count >= 2 {
                TrackRouteSketch(points: details.shape)
                    .padding(22)
            } else if details != nil {
                Text("Not enough points to draw a route.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Text(track.startedAt.formatted(date: .complete, time: .shortened))
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.textSecondary)
                .padding(12)
        }
        .frame(height: 240)
        .overlay(RoundedRectangle(cornerRadius: Theme.corner, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
    }

    private func statsGrid(_ s: SettingsTrackSummary, units: DistanceUnits) -> some View {
        LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 12) {
            TrackStatTile(title: "Distance", value: GeoFormat.distance(s.distance, units: units), symbol: "arrow.left.and.right")
            TrackStatTile(title: "Duration", value: GeoFormat.duration(s.duration), symbol: "clock.fill")
            TrackStatTile(title: "Ascent", value: GeoFormat.elevation(s.ascent, units: units), symbol: "arrow.up.right")
            TrackStatTile(title: "Avg speed", value: s.averageSpeed.map { GeoFormat.speed($0, units: units) } ?? "–", symbol: "speedometer")
            TrackStatTile(title: "Highest", value: s.maxElevation.map { GeoFormat.elevation($0, units: units) } ?? "–", symbol: "mountain.2.fill")
            TrackStatTile(title: "Points", value: "\(s.pointCount)", symbol: "point.3.connected.trianglepath.dotted")
        }
    }

    private func profileCard(_ d: SettingsTrackDetails, units: DistanceUnits) -> some View {
        let toX = units == .metric ? 1.0 / 1000 : 1.0 / 1609.344
        let toY = units == .metric ? 1.0 : 3.28084
        let lo = (d.summary.minElevation ?? 0) * toY
        let hi = (d.summary.maxElevation ?? 0) * toY
        let pad = max(5, (hi - lo) * 0.12)
        let base = lo - pad
        let xUnit: String = units == .metric ? "km" : "mi"
        let yUnit: String = units == .metric ? "m" : "ft"
        return VStack(alignment: .leading, spacing: 10) {
            Text("Elevation")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
            Chart(d.profile) { p in
                AreaMark(
                    x: .value("Distance", p.distance * toX),
                    yStart: .value("Base", base),
                    yEnd: .value("Elevation", p.elevation * toY)
                )
                .foregroundStyle(LinearGradient(colors: [Theme.track.opacity(0.45), Theme.track.opacity(0.02)],
                                                startPoint: .top, endPoint: .bottom))
                .interpolationMethod(.catmullRom)
                LineMark(
                    x: .value("Distance", p.distance * toX),
                    y: .value("Elevation", p.elevation * toY)
                )
                .foregroundStyle(Theme.track)
                .lineStyle(StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
                .interpolationMethod(.catmullRom)
            }
            .chartYScale(domain: base...(hi + pad))
            .chartXAxisLabel(xUnit)
            .chartYAxisLabel(yUnit)
            .frame(height: 170)
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.corner, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
    }
}

/// Everything the detail page draws, computed once off the main thread.
private struct SettingsTrackDetails {
    struct ProfilePoint: Identifiable {
        let id: Int
        /// Metres along the track.
        let distance: Double
        /// Metres.
        let elevation: Double
    }

    let summary: SettingsTrackSummary
    /// Route shape in a unit square (x right, y down, north up), aspect preserved.
    let shape: [CGPoint]
    let profile: [ProfilePoint]

    init(_ track: Track, maxShapePoints: Int = 800, maxProfilePoints: Int = 300) {
        summary = SettingsTrackSummary(track)
        let points = track.points

        var shape: [CGPoint] = []
        if points.count >= 2 {
            let step = max(1, points.count / maxShapePoints)
            var merc: [SIMD2<Double>] = []
            merc.reserveCapacity(points.count / step + 2)
            var i = 0
            while i < points.count {
                merc.append(WebMercator.project(points[i].latLon))
                i += step
            }
            if let last = points.last, (points.count - 1) % step != 0 {
                merc.append(WebMercator.project(last.latLon))
            }
            var minP = SIMD2<Double>(.infinity, .infinity)
            var maxP = SIMD2<Double>(-.infinity, -.infinity)
            for m in merc {
                minP = SIMD2(min(minP.x, m.x), min(minP.y, m.y))
                maxP = SIMD2(max(maxP.x, m.x), max(maxP.y, m.y))
            }
            let span = max(maxP.x - minP.x, maxP.y - minP.y, 1)
            let center = (minP + maxP) / 2
            shape = merc.map { m in
                CGPoint(x: 0.5 + (m.x - center.x) / span, y: 0.5 - (m.y - center.y) / span)
            }
        }
        self.shape = shape

        var profile: [ProfilePoint] = []
        let withAltitude = points.reduce(0) { $0 + ($1.altitude == nil ? 0 : 1) }
        if withAltitude >= 2 {
            let step = max(1, withAltitude / maxProfilePoints)
            var along = 0.0
            var seen = 0
            for (i, p) in points.enumerated() {
                if i > 0 {
                    along += Geodesy.distance(points[i - 1].latLon, p.latLon)
                }
                guard let a = p.altitude, a.isFinite else { continue }
                if seen % step == 0 {
                    profile.append(ProfilePoint(id: profile.count, distance: along, elevation: a))
                }
                seen += 1
            }
        }
        self.profile = profile
    }
}

/// Dotted route sketch with start (green) and end (red) markers.
private struct TrackRouteSketch: View {
    let points: [CGPoint]

    var body: some View {
        Canvas { context, size in
            let side = min(size.width, size.height)
            let origin = CGPoint(x: (size.width - side) / 2, y: (size.height - side) / 2)
            func toView(_ p: CGPoint) -> CGPoint {
                CGPoint(x: origin.x + p.x * side, y: origin.y + p.y * side)
            }
            var path = Path()
            path.move(to: toView(points[0]))
            for p in points.dropFirst() {
                path.addLine(to: toView(p))
            }
            context.stroke(path, with: .color(Theme.track.opacity(0.25)),
                           style: StrokeStyle(lineWidth: 8, lineCap: .round, lineJoin: .round))
            context.stroke(path, with: .color(Theme.track),
                           style: StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round, dash: [0.5, 6]))
            if let first = points.first, let last = points.last {
                let start = toView(first)
                let end = toView(last)
                context.fill(Path(ellipseIn: CGRect(x: start.x - 6, y: start.y - 6, width: 12, height: 12)), with: .color(Theme.good))
                context.fill(Path(ellipseIn: CGRect(x: end.x - 7, y: end.y - 7, width: 14, height: 14)), with: .color(Color.white))
                context.fill(Path(ellipseIn: CGRect(x: end.x - 5, y: end.y - 5, width: 10, height: 10)), with: .color(Theme.userRed))
            }
        }
        .accessibilityLabel("Route")
    }
}

private struct TrackStatTile: View {
    let title: String
    let value: String
    let symbol: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(title, systemImage: symbol)
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.textSecondary)
            Text(value)
                .font(.system(.title3, design: .rounded).weight(.bold))
                .monospacedDigit()
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
    }
}
