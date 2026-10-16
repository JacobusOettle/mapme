import SwiftUI

// Offline terrain's views (docs/design/explorer.md, Offline regions): the Settings section with
// your regions (progress, pause/resume, swipe to delete), "Add Region…" (the region picker), the
// terrain kept around your maps and its status per map.

/// Settings → Offline terrain: your regions (size, progress, pause/resume, delete), "Add Region…",
/// the terrain kept around your maps and its switch.
@MainActor
struct OfflineSettingsSection: View {
    @Environment(AppModel.self) private var model
    @State private var showPicker = false

    init() {}

    var body: some View {
        @Bindable var settings = model.settings
        let offline = model.offline
        Section {
            ForEach(Array(offline.regions.enumerated()), id: \.element.id) { index, region in
                OfflineRegionRow(region: region, isActive: offline.activeRegionID == region.id,
                                 onPause: { offline.pause(region.id) },
                                 onResume: { offline.resume(region.id) },
                                 onUseMobileData: { offline.allowCellular(region.id) })
                    .accessibilityIdentifier("offlineRegionRow-\(index)")
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        Button(role: .destructive) {
                            withAnimation { offline.delete(region.id) }
                        } label: {
                            Label("Delete", systemImage: "trash")
                        }
                        .accessibilityIdentifier("deleteOfflineRegionButton")
                    }
                    .contextMenu {
                        regionMenu(region, offline: offline)
                    }
            }
            // The cover and the size refresh hang on this row, which is always there: modifiers on
            // a Section inside a Form can end up on every one of its rows.
            Button {
                showPicker = true
            } label: {
                SettingsLabel("Add Region\u{2026}", symbol: "plus", color: Theme.accent.opacity(0.85))
            }
            .accessibilityIdentifier("addOfflineRegionButton")
            .fullScreenCover(isPresented: $showPicker) {
                RegionPickerView()
                    .environment(model)
            }
            .task {
                offline.refreshPinnedBytes()
            }

            Toggle(isOn: $settings.cacheTerrainAroundMaps) {
                SettingsLabel("Keep terrain around my maps", symbol: "mountain.2.fill", color: .brown)
            }
            .accessibilityIdentifier("keepTerrainAroundMapsToggle")

            if settings.cacheTerrainAroundMaps {
                NavigationLink {
                    TrailheadTerrainList()
                } label: {
                    LabeledContent {
                        Text(trailheadSummary(offline))
                            .monospacedDigit()
                    } label: {
                        SettingsLabel("Terrain around your maps", symbol: "map.circle.fill", color: .teal)
                    }
                }
                .accessibilityIdentifier("trailheadTerrainLink")
            }

            Toggle(isOn: $settings.offlineDownloadsOnCellular) {
                SettingsLabel("Download over mobile data", symbol: "antenna.radiowaves.left.and.right", color: .green)
            }
            .accessibilityIdentifier("offlineCellularToggle")
        } header: {
            Text("Offline terrain")
        } footer: {
            Text(footer(offline))
                .accessibilityIdentifier("offlineTerrainFooter")
        }
    }

    @ViewBuilder
    private func regionMenu(_ region: OfflineRegion, offline: OfflineRegionService) -> some View {
        switch region.status {
        case .downloading, .queued:
            Button {
                offline.pause(region.id)
            } label: {
                Label("Pause", systemImage: "pause.fill")
            }
        case .paused, .failed:
            Button {
                offline.resume(region.id)
            } label: {
                Label("Resume", systemImage: "arrow.down.circle")
            }
        case .complete:
            if region.tilesFailed > 0 {
                Button {
                    offline.resume(region.id)
                } label: {
                    Label("Retry Missing Tiles", systemImage: "arrow.clockwise")
                }
            }
        }
        Button(role: .destructive) {
            offline.delete(region.id)
        } label: {
            Label("Delete", systemImage: "trash")
        }
    }

    private func trailheadSummary(_ offline: OfflineRegionService) -> String {
        let records = offline.trailheads.values
        let bytes = records.reduce(Int64(0)) { $0 + $1.bytesOnDisk }
        let maps = records.filter { $0.tilesDone > 0 }.count
        if records.contains(where: { $0.status == .downloading }) { return "Saving\u{2026}" }
        guard maps > 0 else { return records.isEmpty ? "None yet" : "Waiting" }
        return "\(maps == 1 ? "1 map" : "\(maps) maps") \u{00B7} \(OfflineText.size(bytes: bytes))"
    }

    private func footer(_ offline: OfflineRegionService) -> String {
        var text = "Download the terrain of an area before you go: MapMe then finds and locates maps, shows elevation profiles, the terrain beyond your maps and 3D without a connection. Standard detail is enough to locate maps; High detail locates them as precisely as online."
        if model.settings.cacheTerrainAroundMaps {
            text += " The terrain around every map you locate is kept too (usually 3\u{2013}14\u{00A0}MB), and deleted with the map."
        }
        if #available(iOS 26.0, *) {
            text += " Downloads carry on when you leave MapMe."
        } else {
            text += " Downloads carry on for a short while after you leave MapMe, then in batches, and resume when you open it."
        }
        if let bytes = offline.pinnedBytes, bytes > 0 {
            text += " Offline terrain uses \(OfflineText.size(bytes: bytes)) on this iPhone."
        }
        return text
    }
}

/// One offline region: name, detail, progress or size and date, and a pause/resume button.
struct OfflineRegionRow: View {
    let region: OfflineRegion
    let isActive: Bool
    var onPause: () -> Void
    var onResume: () -> Void
    var onUseMobileData: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            SymbolTile(symbol: symbol, color: tint)
            VStack(alignment: .leading, spacing: 4) {
                Text(region.name)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                Text(statusLine)
                    .font(.caption)
                    .foregroundStyle(region.status == .failed ? Theme.bad : Theme.textSecondary)
                    .monospacedDigit()
                    .lineLimit(2)
                    .accessibilityIdentifier("offlineRegionStatus")
                if showsProgress {
                    ProgressView(value: region.fraction)
                        .tint(region.status == .paused ? Theme.warn : Theme.accent)
                        .animation(.linear(duration: 0.25), value: region.fraction)
                }
                if region.status == .paused, region.pauseReason == .wifi {
                    Button("Use Mobile Data", action: onUseMobileData)
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("offlineUseMobileDataButton")
                }
            }
            Spacer(minLength: 4)
            actionButton
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var actionButton: some View {
        switch region.status {
        case .downloading, .queued:
            roundButton("pause.fill", label: "Pause", id: "pauseOfflineRegionButton", action: onPause)
        case .paused, .failed:
            roundButton("arrow.down", label: "Resume", id: "resumeOfflineRegionButton", action: onResume)
        case .complete:
            if region.tilesFailed > 0 {
                roundButton("arrow.clockwise", label: "Retry missing tiles", id: "retryOfflineRegionButton", action: onResume)
            } else {
                Image(systemName: "checkmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(Theme.good)
                    .accessibilityLabel("Downloaded")
            }
        }
    }

    private func roundButton(_ symbol: String, label: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .bold))
                .foregroundStyle(Theme.accent)
                .frame(width: 32, height: 32)
                .background(Theme.accent.opacity(0.15), in: Circle())
                .frame(width: Theme.minTapTarget, height: Theme.minTapTarget)
                .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .accessibilityLabel(label)
        .accessibilityIdentifier(id)
    }

    private var showsProgress: Bool {
        region.status == .downloading || ((region.status == .paused || region.status == .queued) && region.tilesDone > 0)
    }

    private var symbol: String {
        switch region.status {
        case .queued: return "clock.fill"
        case .downloading: return "arrow.down"
        case .paused: return "pause.fill"
        case .complete: return "mountain.2.fill"
        case .failed: return "exclamationmark.triangle.fill"
        }
    }

    private var tint: Color {
        switch region.status {
        case .queued: return .gray
        case .downloading: return Theme.accent.opacity(0.85)
        case .paused: return .orange
        case .complete: return region.tilesFailed > 0 ? .orange : .brown
        case .failed: return .red
        }
    }

    /// "Standard · 1 204 of 2 475 tiles · 41 %", "High detail · 342 MB · 12 Oct 2026".
    var statusLine: String {
        Self.statusLine(region)
    }

    static func statusLine(_ region: OfflineRegion) -> String {
        let percent = "\(Int((region.fraction * 100).rounded(.down))) %"
        var parts = [region.detail.label]
        switch region.status {
        case .queued:
            parts.append(region.tilesDone > 0 ? "Waiting to resume \u{00B7} \(percent)" : "Waiting to start")
        case .downloading:
            if region.tilesTotal > 0 {
                parts.append("\(SearchText.grouped(region.tilesDone)) of \(SearchText.grouped(region.tilesTotal)) tiles")
                parts.append(percent)
            } else {
                parts.append("Starting\u{2026}")
            }
        case .paused:
            parts.append((region.pauseReason ?? .user).label)
            if region.tilesDone > 0 { parts.append(percent) }
        case .complete:
            parts.append(OfflineText.size(bytes: region.bytesOnDisk))
            if let date = region.completedAt { parts.append(date.formatted(date: .abbreviated, time: .omitted)) }
            if let message = region.message { parts.append(message) }
        case .failed:
            parts.append(region.message ?? "Couldn\u{2019}t download")
        }
        return parts.joined(separator: " \u{00B7} ")
    }
}

/// A symbol on a tinted rounded square, as Settings rows have.
private struct SymbolTile: View {
    let symbol: String
    let color: Color

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Color.white)
            .frame(width: 28, height: 28)
            .background(color.gradient, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            .accessibilityHidden(true)
    }
}

/// Settings → Offline terrain → Terrain around your maps: per located map, what is kept around it.
struct TrailheadTerrainList: View {
    @Environment(AppModel.self) private var model

    init() {}

    var body: some View {
        let maps = model.library.maps.filter(\.isGeoreferenced)
        List {
            Section {
                if maps.isEmpty {
                    Text("Locate a map and the terrain around it is saved here.")
                        .foregroundStyle(Theme.textSecondary)
                }
                ForEach(maps) { map in
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(map.name)
                                .foregroundStyle(Theme.textPrimary)
                                .lineLimit(1)
                            Text(TrailheadStatus.text(model.offline.trailhead(for: map.id)))
                                .font(.caption)
                                .foregroundStyle(Theme.textSecondary)
                                .monospacedDigit()
                        }
                        Spacer(minLength: 8)
                        TrailheadStatusChip(record: model.offline.trailhead(for: map.id))
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("trailheadRow")
                }
            } footer: {
                Text("Around each map you locate, MapMe keeps the terrain to locate nearby maps (50 km), the terrain beyond the sheet and its 3D view, on this iPhone. It is downloaded right after the map is located, or when you are next online, and deleted with the map.")
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle("Terrain Around Maps")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// Wording of a map's trailhead terrain.
enum TrailheadStatus {
    /// "Saved · 8.2 MB", "Saving… 40 %", "Waiting for network", "Not saved".
    static func text(_ record: TrailheadTerrain?) -> String {
        guard let record else { return "Not saved" }
        switch record.status {
        case .complete:
            var text = "Saved \u{00B7} \(OfflineText.size(bytes: record.bytesOnDisk))"
            if record.tilesFailed > 0 { text += " \u{00B7} \(OfflineText.tiles(record.tilesFailed)) missing" }
            return text
        case .downloading:
            return "Saving\u{2026} \(Int((record.fraction * 100).rounded(.down))) %"
        case .pending:
            return record.message ?? "Waiting to save"
        case .failed:
            return record.message ?? "Couldn\u{2019}t save"
        }
    }

    static func symbol(_ record: TrailheadTerrain?) -> String {
        switch record?.status {
        case .complete: return "checkmark.circle.fill"
        case .downloading: return "arrow.down.circle.fill"
        case .pending: return "clock.fill"
        case .failed: return "exclamationmark.triangle.fill"
        case nil: return "circle.dashed"
        }
    }

    static func color(_ record: TrailheadTerrain?) -> Color {
        switch record?.status {
        case .complete: return (record?.tilesFailed ?? 0) > 0 ? Theme.warn : Theme.good
        case .downloading: return Theme.accent
        case .pending: return Theme.textSecondary
        case .failed: return Theme.bad
        case nil: return Theme.textSecondary
        }
    }

    /// The short form for a map card: "Terrain offline", "Saving terrain 40 %", "Terrain waits".
    static func short(_ record: TrailheadTerrain?) -> String? {
        guard let record else { return nil }
        switch record.status {
        case .complete: return "Terrain offline"
        case .downloading: return "Saving terrain \(Int((record.fraction * 100).rounded(.down))) %"
        case .pending: return "Terrain waits"
        case .failed: return nil
        }
    }
}

/// A capsule with a map's trailhead terrain status (icon and short text).
struct TrailheadStatusChip: View {
    let record: TrailheadTerrain?

    var body: some View {
        let color = TrailheadStatus.color(record)
        Image(systemName: TrailheadStatus.symbol(record))
            .font(.body.weight(.semibold))
            .foregroundStyle(color)
            .accessibilityLabel(TrailheadStatus.text(record))
    }
}

/// For a map card in the library: "Terrain offline" / "Saving terrain 40 %" with a symbol, nothing
/// when no terrain is kept around the map. Reads the app model from the environment.
struct TrailheadStatusLabel: View {
    @Environment(AppModel.self) private var model
    let mapID: UUID

    init(mapID: UUID) {
        self.mapID = mapID
    }

    var body: some View {
        let record = model.offline.trailhead(for: mapID)
        if let text = TrailheadStatus.short(record) {
            Label(text, systemImage: TrailheadStatus.symbol(record))
                .font(.caption.weight(.semibold))
                .foregroundStyle(TrailheadStatus.color(record))
                .monospacedDigit()
                .accessibilityIdentifier("trailheadStatusLabel")
        }
    }
}
