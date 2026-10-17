import CoreLocation
import SwiftUI
import UIKit

/// AllTrails / Apple Maps style bottom panel. Collapsed: elevation, trail distance and time, and
/// the big record button (or where the map is when you are off it). Expanded: speed, accuracy,
/// heading, ascent, coordinates (long-press to copy) and the nearest points of interest.
/// Drag it, or tap its top, to switch between the two detents.
@MainActor
struct TrailStatsPanel: View {
    let map: MapDocument
    @Binding var expanded: Bool
    /// Height available for the expanded details (they scroll beyond it).
    var maxDetailsHeight: CGFloat
    var onShowAll: () -> Void
    var onSelectPOI: (MapPOI) -> Void

    @GestureState(resetTransaction: Transaction(animation: .spring(response: 0.35, dampingFraction: 0.85)))
    private var drag: CGFloat = 0
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            header
            if expanded {
                TrailStatsDetails(map: map, maxHeight: maxDetailsHeight, onShowAll: onShowAll, onSelectPOI: onSelectPOI,
                                  onSelectItem: { ref in
                                      // Your waypoints and notes are selected by the map screen.
                                      setExpanded(false)
                                      model.navigator.screen.selectionRequest = ref
                                  })
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .padding(.horizontal, 18)
        .padding(.bottom, 14)
        .frame(maxWidth: 520)
        .floatingChrome(RoundedRectangle(cornerRadius: 26, style: .continuous))
        .offset(y: dragOffset)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("statsPanel")
        .sensoryFeedback(.selection, trigger: expanded)
    }

    /// Follows the finger toward the other detent, rubber-bands the other way.
    private var dragOffset: CGFloat {
        if drag > 0 { return expanded ? drag : drag * 0.3 }
        return drag * 0.25
    }

    private var header: some View {
        VStack(spacing: 8) {
            Capsule()
                .fill(Color.primary.opacity(0.28))
                .frame(width: 36, height: 5)
                .padding(.top, 7)
                .accessibilityLabel(expanded ? "Collapse details" : "Expand details")
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { setExpanded(!expanded) }
            HStack(alignment: .center, spacing: 12) {
                TrailStatsRow(map: map)
                    .frame(maxWidth: .infinity, alignment: .leading)
                RecordButton()
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { setExpanded(!expanded) }
        .simultaneousGesture(dragGesture)
    }

    private var dragGesture: some Gesture {
        DragGesture(minimumDistance: 6)
            .updating($drag) { value, state, _ in
                state = value.translation.height
            }
            .onEnded { value in
                let dy = value.translation.height
                let predicted = value.predictedEndTranslation.height
                if !expanded, dy < -24 || predicted < -90 {
                    setExpanded(true)
                } else if expanded, dy > 24 || predicted > 90 {
                    setExpanded(false)
                }
            }
    }

    private func setExpanded(_ value: Bool) {
        guard value != expanded else { return }
        withAnimation(.spring(response: 0.42, dampingFraction: 0.86)) { expanded = value }
    }
}

// MARK: - Collapsed row

/// ELEVATION | DISTANCE | TIME, or why there are no stats (no fix, no permission, off the map).
@MainActor
private struct TrailStatsRow: View {
    let map: MapDocument
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL

    var body: some View {
        let auth = model.location.authorization
        let units = model.settings.units
        if auth == .denied || auth == .restricted {
            HStack(spacing: 10) {
                Image(systemName: "location.slash.fill")
                    .foregroundStyle(Theme.warn)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Location access is off")
                        .font(.subheadline.weight(.semibold))
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                    .font(.footnote.weight(.semibold))
                }
            }
        } else if let fix = model.location.location {
            if let away = MapProximity.offMapText(map: map, here: LatLon(fix.coordinate), units: units) {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.up.forward.circle.fill")
                        .font(.title2)
                        .foregroundStyle(Theme.warn)
                        .symbolRenderingMode(.hierarchical)
                    Text(away)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("offMapNotice")
            } else {
                HStack(alignment: .top, spacing: 8) {
                    PanelStat(caption: "Elevation",
                              value: fix.verticalAccuracy > 0 ? GeoFormat.elevation(fix.altitude, units: units) : "—")
                    PanelStat(caption: "Distance", value: GeoFormat.distance(model.tracks.current.distance, units: units))
                    TrailTimeStat()
                }
            }
        } else {
            HStack(spacing: 10) {
                ProgressView()
                    .controlSize(.small)
                Text("Waiting for GPS…")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

extension MapProximity {
    /// "You are 2.3 km NE of this map" when you are outside the map's footprint.
    static func offMapText(map: MapDocument, here: LatLon, units: DistanceUnits) -> String? {
        guard case .away(let meters, let bearing) = MapProximity.of(map, from: here) else { return nil }
        // The proximity bearing points from you to the map; this sentence needs the reverse.
        let fromMap = (bearing + 180).truncatingRemainder(dividingBy: 360)
        return "You are \(GeoFormat.distance(meters, units: units)) \(Geodesy.compassPoint(fromMap)) of this map"
    }
}

/// Recording time, or how long ago the on-screen trail started. Ticks once a second.
@MainActor
private struct TrailTimeStat: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let recorder = model.tracks
            let start = recorder.isRecording ? recorder.current.startedAt : recorder.current.points.first?.timestamp
            let elapsed = start.map { max(0, context.date.timeIntervalSince($0)) } ?? 0
            PanelStat(caption: "Time", value: GeoFormat.duration(elapsed), tint: recorder.isRecording ? Theme.record : nil)
        }
    }
}

/// Small-caps caption over a big rounded number with its unit.
struct PanelStat: View {
    let caption: String
    let value: String
    var tint: Color? = nil

    var body: some View {
        let parts = Self.split(value)
        VStack(alignment: .leading, spacing: 1) {
            Text(caption.uppercased())
                .font(.caption2.weight(.semibold))
                .kerning(0.6)
                .foregroundStyle(tint ?? Color.secondary)
                .lineLimit(1)
            HStack(alignment: .firstTextBaseline, spacing: 2) {
                Text(parts.number)
                    .font(.system(.title2, design: .rounded).weight(.semibold))
                    .monospacedDigit()
                    .foregroundStyle(.primary)
                    .contentTransition(.numericText())
                if !parts.unit.isEmpty {
                    Text(parts.unit)
                        .font(.system(.footnote, design: .rounded).weight(.semibold))
                        .foregroundStyle(.secondary)
                }
            }
            .lineLimit(1)
            .minimumScaleFactor(0.6)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    /// "1.25 km" → ("1.25", "km"); "0:42" → ("0:42", "").
    static func split(_ value: String) -> (number: String, unit: String) {
        guard let space = value.lastIndex(of: " ") else { return (value, "") }
        let unit = value[value.index(after: space)...]
        guard !unit.isEmpty, unit.allSatisfy({ $0.isLetter || $0 == "/" }) else { return (value, "") }
        return (String(value[..<space]), String(unit))
    }
}

// MARK: - Expanded details

@MainActor
private struct TrailStatsDetails: View {
    let map: MapDocument
    var maxHeight: CGFloat
    var onShowAll: () -> Void
    var onSelectPOI: (MapPOI) -> Void
    var onSelectItem: (GeoItemRef) -> Void

    var body: some View {
        ViewThatFits(in: .vertical) {
            content
            ScrollView {
                content
            }
            .scrollIndicators(.hidden)
        }
        .frame(maxHeight: max(160, maxHeight))
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 14) {
            Rectangle()
                .fill(Color.primary.opacity(0.12))
                .frame(height: 0.5)
                .padding(.top, 12)
            LiveDetailGrid()
            TrailProfileSection()
            CoordinateRow()
            NearbyPreview(map: map, onShowAll: onShowAll, onSelectPOI: onSelectPOI, onSelectItem: onSelectItem)
        }
    }
}

@MainActor
private struct LiveDetailGrid: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let units = model.settings.units
        let fix = model.location.location
        Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 12) {
            GridRow {
                PanelStat(caption: "Speed", value: fix.map { Self.speed($0, units: units) } ?? "—")
                PanelStat(caption: "Accuracy", value: fix.map { Self.accuracy($0, units: units) } ?? "—")
            }
            GridRow {
                HeadingStat()
                PanelStat(caption: "Ascent", value: GeoFormat.elevation(model.tracks.current.ascent, units: units))
            }
        }
    }

    private static func speed(_ fix: CLLocation, units: DistanceUnits) -> String {
        GeoFormat.speed(max(0, fix.speed), units: units)
    }

    private static func accuracy(_ fix: CLLocation, units: DistanceUnits) -> String {
        guard fix.horizontalAccuracy >= 0 else { return "—" }
        return "±" + GeoFormat.distance(max(1, fix.horizontalAccuracy), units: units)
    }
}

/// Reads the heading itself so compass updates redraw only this stat.
@MainActor
private struct HeadingStat: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let h = model.location.headingDegrees {
            let deg = Int(h.rounded()) % 360
            PanelStat(caption: "Heading", value: "\(deg)° \(Geodesy.compassPoint(h))")
        } else {
            PanelStat(caption: "Heading", value: "—")
        }
    }
}

/// Your coordinates in the chosen format; long-press (or the Copy action) copies them.
@MainActor
private struct CoordinateRow: View {
    @Environment(AppModel.self) private var model
    @State private var copied = false
    @State private var copyTick = 0

    var body: some View {
        let text = model.location.location.map {
            GeoFormat.coordinate(LatLon($0.coordinate), format: model.settings.coordinateFormat)
        }
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("COORDINATES")
                    .font(.caption2.weight(.semibold))
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                Text(text ?? "—")
                    .font(.system(.callout, design: .monospaced).weight(.medium))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
            }
            Spacer(minLength: 6)
            Image(systemName: copied ? "checkmark.circle.fill" : "doc.on.doc")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(copied ? Theme.good : Color.secondary)
                .contentTransition(.symbolEffect(.replace))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(Color.primary.opacity(0.07), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .onLongPressGesture(minimumDuration: 0.4) {
            if let text { copy(text) }
        }
        .sensoryFeedback(.success, trigger: copyTick)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Long-press to copy")
        .accessibilityAction(named: "Copy") {
            if let text { copy(text) }
        }
    }

    private func copy(_ text: String) {
        UIPasteboard.general.string = text
        copyTick += 1
        withAnimation(.snappy) { copied = true }
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_600_000_000)
            withAnimation(.snappy) { copied = false }
        }
    }
}

/// The elevation profile of the trail you walked this session (live): from the terrain model when
/// there is terrain, GPS altitude otherwise, and it says which (docs/design/explorer.md, Profile).
@MainActor
private struct TrailProfileSection: View {
    @Environment(AppModel.self) private var model
    @State private var result: RouteProfile?

    var body: some View {
        let points = model.tracks.current.points
        // Worked out again every ten new points (about a minute of walking), not on every fix.
        let bucket = points.count / 10
        VStack(alignment: .leading, spacing: 6) {
            Text("Elevation")
                .font(.headline)
                .foregroundStyle(.primary)
            if let result, points.count >= 2, result.hasElevation {
                ElevationProfileView(profile: result.profile, source: result.source, units: model.settings.units,
                                     color: Theme.track, height: 110)
            } else {
                Text(points.count >= 2 && result != nil
                     ? "No elevation for this trail yet: the terrain isn't on your iPhone and GPS gave no altitude."
                     : "Your trail's elevation profile appears as you walk.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("trailProfile")
        .task(id: bucket) {
            guard points.count >= 2 else {
                result = nil
                return
            }
            result = await RoutePlanner.trackProfile(of: points, terrain: model.terrain)
        }
    }
}

/// The five nearest points of interest (with your waypoints and notes) and "Show All".
@MainActor
private struct NearbyPreview: View {
    let map: MapDocument
    var onShowAll: () -> Void
    var onSelectPOI: (MapPOI) -> Void
    var onSelectItem: (GeoItemRef) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let here = model.location.location.map { LatLon($0.coordinate) }
        let entries = Array(NearbyEntry.merged(pois: NearbyRow.rows(for: map.pois, from: here),
                                               items: GeoItemRow.rows(store: model.geoItems, near: map, from: here))
            .prefix(5))
        let units = model.settings.units
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("Nearby")
                    .font(.headline)
                    .foregroundStyle(.primary)
                Spacer()
                Button(action: onShowAll) {
                    HStack(spacing: 3) {
                        Text("Show All")
                        Image(systemName: "chevron.right")
                            .font(.caption.weight(.bold))
                    }
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.blue)
                    .padding(.vertical, 4)
                    .contentShape(Rectangle())
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.95))
                .accessibilityIdentifier("nearbyButton")
            }
            if entries.isEmpty {
                Text("Names read off the map, OpenStreetMap places and the waypoints you drop appear here.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(entries.enumerated()), id: \.element.id) { index, entry in
                        switch entry {
                        case .poi(let row):
                            Button {
                                onSelectPOI(row.poi)
                            } label: {
                                NearbyRowView(row: row, units: units)
                            }
                            .buttonStyle(NearbyRowButtonStyle())
                        case .item(let item):
                            Button {
                                onSelectItem(item.ref)
                            } label: {
                                GeoItemRowView(row: item, units: units)
                            }
                            .buttonStyle(NearbyRowButtonStyle())
                        }
                        if index < entries.count - 1 {
                            Rectangle()
                                .fill(Color.primary.opacity(0.1))
                                .frame(height: 0.5)
                                .padding(.leading, 62)
                        }
                    }
                }
                .background(Color.primary.opacity(0.07), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            }
        }
    }
}
