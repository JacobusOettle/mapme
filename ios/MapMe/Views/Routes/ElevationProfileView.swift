import Charts
import SwiftUI

/// A named point shown on a profile (a waypoint near the route).
struct ProfileMarker: Identifiable, Equatable, Sendable {
    var id: UUID
    var name: String
    /// Metres along the profile.
    var distanceM: Double
}

/// An elevation profile (docs/design/explorer.md, Profile): the samples as an area and a line,
/// y from the lowest point − 5 % of the range to the highest + 5 %, axes in your units, your
/// position while following, waypoints near the route, and drag to scrub (the map shows the
/// scrubbed point). Says where the elevations came from.
struct ElevationProfileView: View {
    let profile: ElevationProfile
    var source: ProfileSource = .terrain
    var units: DistanceUnits = .metric
    var color: Color = Theme.route
    /// Metres along the profile where you are (following).
    var progressM: Double? = nil
    var markers: [ProfileMarker] = []
    var height: CGFloat = 96
    var showsCaption = true
    /// The scrubbed distance (metres along), nil when the finger lifts.
    var onScrub: ((Double?) -> Void)? = nil

    @State private var selectedX: Double?

    private struct Point: Identifiable {
        let id: Int
        let x: Double
        let y: Double
    }

    /// Display units per metre: km or mi along, m or ft up.
    private var xPerMeter: Double { units == .metric ? 1.0 / 1000 : 1.0 / 1609.344 }
    private var yPerMeter: Double { units == .metric ? 1.0 : 3.28084 }
    private var xUnit: String { units == .metric ? "km" : "mi" }
    private var yUnit: String { units == .metric ? "m" : "ft" }

    var body: some View {
        let points = chartPoints
        VStack(alignment: .leading, spacing: 4) {
            if points.count >= 2 {
                chart(points)
                    .frame(height: height)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibilitySummary)
            } else {
                HStack(spacing: 8) {
                    Image(systemName: "mountain.2")
                        .foregroundStyle(.secondary)
                    Text(ProfileSource.none.caption)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: min(height, 44), alignment: .leading)
            }
            if showsCaption, points.count >= 2 {
                Text(source.caption)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .onChange(of: selectedX) { _, x in
            onScrub?(x.map { $0 / xPerMeter })
        }
    }

    private var chartPoints: [Point] {
        // At most ~400 marks: Charts draws each one.
        let samples = profile.samples.filter { $0.elevationM != nil }
        let step = max(1, samples.count / 400)
        var out: [Point] = []
        out.reserveCapacity(samples.count / step + 2)
        for (i, s) in samples.enumerated() where i % step == 0 || i == samples.count - 1 {
            guard let e = s.elevationM else { continue }
            out.append(Point(id: i, x: s.distanceM * xPerMeter, y: e * yPerMeter))
        }
        return out
    }

    private func yDomain(_ points: [Point]) -> ClosedRange<Double> {
        let lo = points.map(\.y).min() ?? 0
        let hi = points.map(\.y).max() ?? 1
        let pad = max((hi - lo) * 0.05, units == .metric ? 5 : 15)
        return (lo - pad)...(hi + pad)
    }

    @ViewBuilder
    private func chart(_ points: [Point]) -> some View {
        let domain = yDomain(points)
        let xMax = max(points.last?.x ?? 1, 1e-6)
        let base = domain.lowerBound
        Chart {
            ForEach(points) { p in
                AreaMark(x: .value("Distance", p.x), yStart: .value("Base", base), yEnd: .value("Elevation", p.y))
                    .foregroundStyle(LinearGradient(colors: [color.opacity(0.45), color.opacity(0.03)],
                                                    startPoint: .top, endPoint: .bottom))
                    .interpolationMethod(.monotone)
                LineMark(x: .value("Distance", p.x), y: .value("Elevation", p.y))
                    .foregroundStyle(color)
                    .lineStyle(StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
                    .interpolationMethod(.monotone)
            }
            ForEach(markers) { m in
                if let e = RoutePlanner.elevation(in: profile, atDistance: m.distanceM) {
                    PointMark(x: .value("Distance", m.distanceM * xPerMeter), y: .value("Elevation", e * yPerMeter))
                        .foregroundStyle(Color.white)
                        .symbolSize(28)
                        .annotation(position: .top, spacing: 2) {
                            Text(m.name)
                                .font(.system(size: 9, weight: .semibold))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                }
            }
            if let progressM, let e = RoutePlanner.elevation(in: profile, atDistance: progressM) {
                RuleMark(x: .value("You", progressM * xPerMeter))
                    .foregroundStyle(Theme.userRed.opacity(0.45))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                PointMark(x: .value("You", progressM * xPerMeter), y: .value("Elevation", e * yPerMeter))
                    .foregroundStyle(Theme.userRed)
                    .symbolSize(70)
            }
            if let selectedX, let e = RoutePlanner.elevation(in: profile, atDistance: selectedX / xPerMeter) {
                RuleMark(x: .value("Scrub", selectedX))
                    .foregroundStyle(Color.primary.opacity(0.4))
                PointMark(x: .value("Scrub", selectedX), y: .value("Elevation", e * yPerMeter))
                    .foregroundStyle(color)
                    .symbolSize(60)
                    .annotation(position: .top, spacing: 4) {
                        Text("\(String(format: "%.2f", selectedX)) \(xUnit) · \(Int((e * yPerMeter).rounded())) \(yUnit)")
                            .font(.caption2.weight(.semibold))
                            .monospacedDigit()
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(.thinMaterial, in: Capsule())
                    }
            }
        }
        .chartXScale(domain: 0...xMax)
        .chartYScale(domain: domain)
        .chartXSelection(value: $selectedX)
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 4)) { value in
                AxisGridLine().foregroundStyle(Color.primary.opacity(0.08))
                AxisValueLabel {
                    if let v = value.as(Double.self) {
                        Text("\(Self.trimmed(v)) \(xUnit)")
                            .font(.caption2)
                    }
                }
            }
        }
        .chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: height > 120 ? 5 : 3)) { value in
                AxisGridLine().foregroundStyle(Color.primary.opacity(0.08))
                AxisValueLabel {
                    if let v = value.as(Double.self) {
                        Text("\(Int(v.rounded()))")
                            .font(.caption2)
                    }
                }
            }
        }
    }

    /// "1.5", "2", "0.25".
    private static func trimmed(_ v: Double) -> String {
        if abs(v - v.rounded()) < 1e-9 { return String(Int(v.rounded())) }
        return String(format: v < 1 ? "%.2f" : "%.1f", v)
    }

    private var accessibilitySummary: String {
        var parts = ["Elevation profile", GeoFormat.distance(profile.distanceM, units: units)]
        if let a = profile.ascentM { parts.append("ascent \(GeoFormat.elevation(a, units: units))") }
        if let d = profile.descentM { parts.append("descent \(GeoFormat.elevation(d, units: units))") }
        if let lo = profile.minM, let hi = profile.maxM {
            parts.append("from \(GeoFormat.elevation(lo, units: units)) to \(GeoFormat.elevation(hi, units: units))")
        }
        return parts.joined(separator: ", ")
    }
}

/// Waypoints within 100 m of a route, as profile markers (at their distance along it).
enum ProfileMarkers {
    static let maxDistanceFromRouteM = 100.0

    static func near(_ route: Route, line: RouteLine, waypoints: [Waypoint], profile: ElevationProfile) -> [ProfileMarker] {
        guard route.points.count >= 2, let bounds = route.bounds?.expanded(byMeters: maxDistanceFromRouteM) else { return [] }
        let scale = line.length > 0 ? profile.distanceM / line.length : 1
        var out: [ProfileMarker] = []
        for w in waypoints where bounds.contains(w.position) {
            guard let m = line.nearest(to: w.position), m.distanceFromLineM <= maxDistanceFromRouteM else { continue }
            out.append(ProfileMarker(id: w.id, name: w.name, distanceM: m.distanceAlongM * scale))
        }
        return out.sorted { $0.distanceM < $1.distanceM }
    }
}

/// A route's line, profile and the waypoints along it, worked out together off the main thread
/// (an imported track can have 100 000 points: nothing here runs per frame or per fix).
struct RouteProfileData: Sendable {
    /// The route and the points this is for.
    let routeID: UUID
    let geometryHash: Int
    let line: RouteLine
    let profile: RouteProfile
    let markers: [ProfileMarker]

    static func make(for route: Route, waypoints: [Waypoint], terrain: TerrainProvider) async -> RouteProfileData? {
        guard let profile = await RoutePlanner.bestProfile(for: route, terrain: terrain) else { return nil }
        let line = RouteLine(route.latLons)
        let markers = ProfileMarkers.near(route, line: line, waypoints: waypoints, profile: profile.profile)
        return RouteProfileData(routeID: route.id, geometryHash: route.geometryHash, line: line, profile: profile,
                                markers: markers)
    }

    /// Still about `route` as it is now (a card shown for another route keeps its state a moment).
    func matches(_ route: Route) -> Bool {
        routeID == route.id && geometryHash == route.geometryHash
    }

    /// Metres along the profile of a distance along the line (the core's and ours agree to the
    /// metre; the scale only absorbs the difference).
    func profileDistance(alongLine meters: Double) -> Double {
        line.length > 0 ? meters * profile.profile.distanceM / line.length : meters
    }

    /// The point on the line at a distance along the profile (the scrubbed point).
    func point(alongProfile meters: Double) -> LatLon? {
        let along = profile.profile.distanceM > 0 ? meters * line.length / profile.profile.distanceM : meters
        return line.point(atDistance: along)
    }
}

/// A route's profile full screen (from the route card): the chart large, the numbers, scrub.
@MainActor
struct RouteProfileScreen: View {
    let routeID: UUID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var data: RouteProfileData?
    @State private var loading = true

    var body: some View {
        let units = model.settings.units
        let route = model.geoItems.route(id: routeID)
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let route {
                        if let data, data.matches(route) {
                            RouteStatsGrid(route: route, profile: data.profile, units: units, pace: model.navigator.currentPace())
                            ElevationProfileView(profile: data.profile.profile, source: data.profile.source, units: units,
                                                 color: route.displayColor.color,
                                                 progressM: progressOnProfile(data),
                                                 markers: data.markers,
                                                 height: 260,
                                                 onScrub: { meters in
                                                     model.navigator.screen.scrubPoint = meters.flatMap { data.point(alongProfile: $0) }
                                                 })
                        } else if loading {
                            ProgressView("Working out the profile…")
                                .frame(maxWidth: .infinity, minHeight: 200)
                        }
                    } else {
                        Text("This route no longer exists.")
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(16)
            }
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle(route?.name ?? "Route")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("profileDoneButton")
                }
            }
        }
        .accessibilityIdentifier("profileFullScreen")
        .task(id: route?.geometryHash) {
            guard let route else { return }
            loading = true
            data = await RouteProfileData.make(for: route, waypoints: model.geoItems.waypoints, terrain: model.terrain)
            loading = false
        }
        .onDisappear { model.navigator.screen.scrubPoint = nil }
    }

    /// Where you are on the profile while following this route.
    private func progressOnProfile(_ data: RouteProfileData) -> Double? {
        guard model.navigator.activeRouteID == routeID, let done = model.navigator.progress?.distanceDoneM else { return nil }
        return data.profileDistance(alongLine: done)
    }
}

/// Distance, climb, descent, walking time (and Naismith's for comparison), highest and lowest.
@MainActor
struct RouteStatsGrid: View {
    let route: Route
    let profile: RouteProfile
    let units: DistanceUnits
    let pace: PaceEstimate

    var body: some View {
        let p = profile.profile
        let time = profile.hasElevation ? PaceModel.movingTime(along: p, factor: pace.factor) : nil
        let flat = PaceModel.flatTime(meters: p.distanceM, factor: pace.factor)
        VStack(alignment: .leading, spacing: 10) {
            Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 12) {
                GridRow {
                    PanelStat(caption: "Distance", value: GeoFormat.distance(p.distanceM, units: units))
                    PanelStat(caption: "Walking", value: RouteFormat.shortDuration(time ?? flat))
                }
                GridRow {
                    PanelStat(caption: "Ascent", value: p.ascentM.map { GeoFormat.elevation($0, units: units) } ?? "—")
                    PanelStat(caption: "Descent", value: p.descentM.map { GeoFormat.elevation($0, units: units) } ?? "—")
                }
                GridRow {
                    PanelStat(caption: "Highest", value: p.maxM.map { GeoFormat.elevation($0, units: units) } ?? "—")
                    PanelStat(caption: "Lowest", value: p.minM.map { GeoFormat.elevation($0, units: units) } ?? "—")
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("Walking time at your pace (\(pace.description)), moving only: breaks are not included.")
                if let ascent = p.ascentM {
                    Text("Naismith's rule: \(RouteFormat.duration(PaceModel.naismithTime(distanceM: p.distanceM, ascentM: ascent))).")
                }
                if time == nil {
                    Text("Without terrain the time assumes flat ground.")
                }
            }
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.corner, style: .continuous))
    }
}
