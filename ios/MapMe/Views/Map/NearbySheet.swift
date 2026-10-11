import CoreLocation
import SwiftUI

/// Bottom sheet listing points of interest on this map, nearest first, with
/// distance and a bearing arrow relative to the user's heading.
struct NearbySheet: View {
    let mapID: UUID
    /// Called when the user taps a POI (the map highlights it and draws a guide line).
    var onSelect: (MapPOI) -> Void
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var category: NearbyFilter = .all
    @State private var renameTarget: MapPOI? = nil
    @State private var renameText = ""
    @State private var showRename = false
    @State private var selectTick = 0

    init(mapID: UUID, onSelect: @escaping (MapPOI) -> Void) {
        self.mapID = mapID
        self.onSelect = onSelect
    }

    var body: some View {
        let fix = model.location.location
        let here = fix.map { LatLon($0.coordinate) }
        let all = model.library.map(id: mapID)?.pois ?? []
        let rows = NearbyRow.rows(for: all.filter { category.includes($0) }, from: here)
        let units = model.settings.units

        VStack(spacing: 0) {
            header(count: all.count, fix: fix, units: units)
                .padding(.horizontal, 20)
                .padding(.top, 20)
                .padding(.bottom, 12)

            filterChips
                .padding(.bottom, 6)

            if all.isEmpty {
                emptyState(
                    title: "No points of interest",
                    symbol: "mappin.slash",
                    text: "Names read off the map, OpenStreetMap places and the waypoints you drop appear here."
                )
            } else if rows.isEmpty {
                emptyState(
                    title: "Nothing here",
                    symbol: category.symbol,
                    text: "This map has no \(category.title.lowercased()) points of interest."
                )
            } else {
                list(rows, units: units)
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(30)
        .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        .nearbySheetBackground()
        .sensoryFeedback(.selection, trigger: selectTick)
        .sensoryFeedback(.selection, trigger: category)
        .alert("Rename waypoint", isPresented: $showRename) {
            TextField("Name", text: $renameText)
            Button("Cancel", role: .cancel) { renameTarget = nil }
            Button("Save") { commitRename() }
        }
    }

    // MARK: - Pieces

    @MainActor
    private func header(count: Int, fix: CLLocation?, units: DistanceUnits) -> some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text("Nearby")
                    .font(.title2.weight(.bold))
                    .foregroundStyle(.primary)
                HStack(spacing: 6) {
                    if let fix {
                        Circle()
                            .fill(Color(uiColor: model.settings.locationColor.uiColor))
                            .frame(width: 7, height: 7)
                        Text(Self.fixText(fix, units: units))
                    } else {
                        Image(systemName: "location.slash")
                            .font(.caption2)
                        Text("Waiting for GPS — map order")
                    }
                    if count > 0 {
                        Text("· \(count) places")
                    }
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
                .monospacedDigit()
            }
            Spacer(minLength: 8)
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(.secondary)
                    .frame(width: 32, height: 32)
                    .background(Color.primary.opacity(0.1), in: Circle())
            }
            .buttonStyle(NearbyRowButtonStyle())
            .accessibilityLabel("Close")
        }
    }

    /// Apple Maps style category chips.
    private var filterChips: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                ForEach(NearbyFilter.allCases) { f in
                    let on = f == category
                    Button {
                        withAnimation(.snappy) { category = f }
                    } label: {
                        Label(f.title, systemImage: f.symbol)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(on ? Color.white : Color.primary)
                            .padding(.horizontal, 14)
                            .frame(height: 34)
                            .background(on ? AnyShapeStyle(Color.blue) : AnyShapeStyle(Color.primary.opacity(0.09)), in: Capsule())
                    }
                    .buttonStyle(NearbyRowButtonStyle())
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
            .padding(.horizontal, 16)
        }
        .scrollIndicators(.hidden)
    }

    private static func fixText(_ fix: CLLocation, units: DistanceUnits) -> String {
        guard fix.horizontalAccuracy >= 0 else { return "From your position" }
        return "From your position · ±" + GeoFormat.distance(fix.horizontalAccuracy, units: units)
    }

    @MainActor
    private func list(_ rows: [NearbyRow], units: DistanceUnits) -> some View {
        List {
            Section {
                ForEach(rows) { row in
                    Button {
                        select(row.poi)
                    } label: {
                        NearbyRowView(row: row, units: units)
                    }
                    .buttonStyle(NearbyRowButtonStyle())
                    .listRowBackground(Color.primary.opacity(0.07))
                    .listRowInsets(EdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 12))
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        if row.poi.source == .user {
                            Button(role: .destructive) {
                                delete(row.poi)
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                            Button {
                                beginRename(row.poi)
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            .tint(Color.blue)
                        }
                    }
                    .contextMenu {
                        Button {
                            select(row.poi)
                        } label: {
                            Label("Show on Map", systemImage: "scope")
                        }
                        if row.poi.source == .user {
                            Button {
                                beginRename(row.poi)
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            Button(role: .destructive) {
                                delete(row.poi)
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .contentMargins(.top, 4, for: .scrollContent)
        .environment(\.defaultMinListRowHeight, 10)
    }

    private func emptyState(title: String, symbol: String, text: String) -> some View {
        VStack(spacing: 10) {
            Spacer(minLength: 16)
            SymbolBadge(symbol: symbol, color: Color.secondary, size: 56)
            Text(title)
                .font(.headline)
                .foregroundStyle(.primary)
            Text(text)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Spacer(minLength: 16)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Actions

    @MainActor
    private func select(_ poi: MapPOI) {
        selectTick += 1
        onSelect(poi)
        dismiss()
    }

    @MainActor
    private func delete(_ poi: MapPOI) {
        guard var doc = model.library.map(id: mapID) else { return }
        withAnimation(.snappy) {
            doc.pois.removeAll { $0.id == poi.id }
            model.library.save(doc)
        }
    }

    @MainActor
    private func beginRename(_ poi: MapPOI) {
        renameTarget = poi
        renameText = poi.name
        showRename = true
    }

    @MainActor
    private func commitRename() {
        defer { renameTarget = nil }
        let name = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let target = renameTarget, !name.isEmpty,
              var doc = model.library.map(id: mapID),
              let i = doc.pois.firstIndex(where: { $0.id == target.id })
        else { return }
        doc.pois[i].name = name
        model.library.save(doc)
    }
}

// MARK: - Model

enum NearbyFilter: String, CaseIterable, Identifiable {
    case all, peaks, water, places, mine

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "All"
        case .peaks: return "Peaks"
        case .water: return "Water"
        case .places: return "Places"
        case .mine: return "Mine"
        }
    }

    var symbol: String {
        switch self {
        case .all: return "mappin.and.ellipse"
        case .peaks: return "mountain.2.fill"
        case .water: return "drop.fill"
        case .places: return "mappin"
        case .mine: return "flag.fill"
        }
    }

    func includes(_ poi: MapPOI) -> Bool {
        switch self {
        case .all: return true
        case .peaks: return poi.kind == .peak || poi.kind == .spotHeight
        case .water: return poi.kind == .water
        case .places:
            switch poi.kind {
            case .place, .parking, .shelter, .viewpoint, .other: return poi.source != .user
            case .peak, .spotHeight, .water, .waypoint: return false
            }
        case .mine: return poi.source == .user
        }
    }
}

/// One POI with its distance and bearing from the user (nil without a fix).
struct NearbyRow: Identifiable, Hashable {
    let poi: MapPOI
    let distance: Double?
    let bearing: Double?

    var id: UUID { poi.id }

    /// Nearest first when `here` is known; otherwise the original order.
    static func rows(for pois: [MapPOI], from here: LatLon?) -> [NearbyRow] {
        guard let here else { return pois.map { NearbyRow(poi: $0, distance: nil, bearing: nil) } }
        let rows = pois.map { poi in
            NearbyRow(poi: poi,
                      distance: Geodesy.distance(here, poi.position),
                      bearing: Geodesy.bearing(here, poi.position))
        }
        return rows.sorted { ($0.distance ?? .infinity) < ($1.distance ?? .infinity) }
    }
}

// MARK: - Row

/// One POI: glyph marker, name, kind/elevation/source, distance and a bearing arrow.
struct NearbyRowView: View {
    let row: NearbyRow
    let units: DistanceUnits

    var body: some View {
        let poi = row.poi
        HStack(spacing: 12) {
            POIGlyphMarker(kind: poi.kind, size: 38)

            VStack(alignment: .leading, spacing: 2) {
                Text(poi.name.isEmpty ? POIKindStyle.name(for: poi.kind) : poi.name)
                    .font(.body.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                Text(subtitle)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            if let distance = row.distance, let bearing = row.bearing {
                VStack(alignment: .trailing, spacing: 2) {
                    Text(distance < 25 ? "Here" : GeoFormat.distance(distance, units: units))
                        .font(.system(.callout, design: .rounded).weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(.primary)
                    Text(Geodesy.compassPoint(bearing))
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.secondary)
                }
                NearbyBearingArrow(bearing: bearing)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var subtitle: String {
        var parts: [String] = []
        if row.poi.kind == .peak || row.poi.kind == .spotHeight || row.poi.kind == .waypoint || row.poi.name.isEmpty {
            parts.append(POIKindStyle.name(for: row.poi.kind))
        }
        if let e = row.poi.elevationM, e.isFinite {
            parts.append(GeoFormat.elevation(e, units: units))
        }
        parts.append(POIKindStyle.sourceName(for: row.poi.source))
        return parts.joined(separator: " · ")
    }
}

/// Arrow pointing at a POI relative to where the phone is facing. Reads the heading
/// itself so heading updates only redraw the arrows, not the whole list.
private struct NearbyBearingArrow: View {
    let bearing: Double
    @Environment(AppModel.self) private var model

    init(bearing: Double) {
        self.bearing = bearing
    }

    var body: some View {
        let heading = model.location.headingDegrees
        let relative = Self.normalized(bearing - (heading ?? 0))
        ZStack {
            Circle()
                .fill(Color.primary.opacity(0.08))
            Image(systemName: "location.north.fill")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(heading == nil ? Color.secondary : Color.blue)
                .rotationEffect(.degrees(relative))
                .animation(.easeOut(duration: 0.25), value: relative)
        }
        .frame(width: 32, height: 32)
        .accessibilityHidden(true)
    }

    /// -180…180 so the arrow turns the short way.
    static func normalized(_ degrees: Double) -> Double {
        var d = degrees.truncatingRemainder(dividingBy: 360)
        if d > 180 { d -= 360 }
        if d <= -180 { d += 360 }
        return d
    }
}

struct NearbyRowButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.75), value: configuration.isPressed)
    }
}

extension View {
    /// Liquid Glass sheet on iOS 26 (the system default); a frosted material before.
    @ViewBuilder
    func nearbySheetBackground() -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            self
        } else {
            self.presentationBackground(.regularMaterial)
        }
        #else
        self.presentationBackground(.regularMaterial)
        #endif
    }
}
