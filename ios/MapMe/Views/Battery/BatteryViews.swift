import SwiftUI

// The battery saver's views (docs/design/explorer.md, Battery saver): the side column's badge
// with what the saver changes, and Settings → Battery.

/// Side column: a small leaf badge while the saver is on; tap for what it changes and the drain
/// measured this session. Empty while the saver is off.
@MainActor
struct BatterySaverBadge: View {
    let context: MapScreenContext
    @Environment(AppModel.self) private var model
    @State private var showInfo = false

    var body: some View {
        if model.battery.isActive {
            Button {
                showInfo = true
            } label: {
                Image(systemName: "leaf.fill")
                    .foregroundStyle(Theme.good)
            }
            .buttonStyle(MapRoundButtonStyle())
            .accessibilityLabel("Battery saver on")
            .accessibilityHint("Shows what the battery saver changes")
            .accessibilityIdentifier("batterySaverBadge")
            .popover(isPresented: $showInfo, arrowEdge: .top) {
                BatterySaverPopover()
                    .presentationCompactAdaptation(.popover)
            }
            .transition(.scale(scale: 0.6).combined(with: .opacity))
        }
    }
}

/// The badge's popover: why the saver is on, what it changes, the drain measured, the mode.
@MainActor
struct BatterySaverPopover: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var settings = model.settings
        let battery = model.battery
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "leaf.fill")
                    .foregroundStyle(Theme.good)
                Text("Battery Saver")
                    .font(.headline)
            }
            Text(battery.statusText)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 7) {
                ForEach(BatterySaverChange.badgeSummary) { change in
                    Label {
                        Text(change.saver)
                            .font(.footnote)
                            .fixedSize(horizontal: false, vertical: true)
                    } icon: {
                        Image(systemName: change.symbol)
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Theme.good)
                            .frame(width: 18)
                    }
                }
            }
            if let drain = battery.drainText {
                Label(drain, systemImage: "battery.50percent")
                    .font(.footnote.weight(.semibold))
                    .monospacedDigit()
                    .accessibilityIdentifier("batteryDrain")
            }
            Text("Typically 10–25 % more time with MapMe open; with the screen off while recording, little (the GPS dominates).")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Picker("Battery saver", selection: $settings.batterySaver) {
                ForEach(BatterySaverMode.allCases) { mode in
                    Text(mode.label).tag(mode)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("batterySaverPopoverPicker")
        }
        .padding(16)
        .frame(width: 300, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("batterySaverPopover")
    }
}

/// Settings → Battery: Off / With Low Power Mode / Always, what it changes, the honest estimate.
@MainActor
struct BatterySettingsSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var settings = model.settings
        let battery = model.battery
        Section {
            Picker(selection: $settings.batterySaver) {
                ForEach(BatterySaverMode.allCases) { mode in
                    Text(mode.label).tag(mode)
                }
            } label: {
                SettingsLabel("Battery saver", symbol: "leaf.fill", color: .green)
            }
            .accessibilityIdentifier("batterySaverPicker")
            LabeledContent {
                Text(battery.statusText)
                    .multilineTextAlignment(.trailing)
            } label: {
                SettingsLabel("Now", symbol: battery.isActive ? "leaf.fill" : "leaf",
                              color: battery.isActive ? .green : .gray)
            }
            .accessibilityIdentifier("batterySaverStatus")
            if let drain = battery.drainText {
                LabeledContent {
                    Text(drain)
                        .monospacedDigit()
                        .multilineTextAlignment(.trailing)
                } label: {
                    SettingsLabel("Battery use", symbol: "battery.50percent", color: .orange)
                }
            }
            NavigationLink {
                BatterySaverDetailsView()
            } label: {
                SettingsLabel("What it changes", symbol: "list.bullet", color: .blue)
            }
            .accessibilityIdentifier("batterySaverDetailsLink")
        } header: {
            Text("Battery")
        } footer: {
            Text(BatterySaver.honestEstimate)
        }
    }
}

/// One thing the battery saver changes, normal → saver.
struct BatterySaverChange: Identifiable {
    let id: String
    let symbol: String
    let title: String
    let normal: String
    let saver: String

    /// docs/design/explorer.md, Battery saver: the table, in words.
    static let all: [BatterySaverChange] = [
        BatterySaverChange(id: "accuracy", symbol: "location.fill", title: "GPS accuracy",
                           normal: "Best available",
                           saver: "To about 10 m. Still the GPS: anything coarser turns it off, which fails in the mountains without Wi-Fi or a cell network."),
        BatterySaverChange(id: "updates", symbol: "point.topleft.down.curvedto.point.bottomright.up", title: "Location updates",
                           normal: "Every fix, about once a second",
                           saver: "After every 5 m you move with the map open, 10 m in the background. Standing still, one fix every 20 s keeps your position current."),
        BatterySaverChange(id: "compass", symbol: "location.north.line.fill", title: "Compass",
                           normal: "1° steps",
                           saver: "5° steps; off in the background and while no map is open."),
        BatterySaverChange(id: "redraw", symbol: "speedometer", title: "Map redraw",
                           normal: "Up to 120 times a second while something moves",
                           saver: "At most 30 times a second. Either way the map is not redrawn while nothing moves."),
        BatterySaverChange(id: "animations", symbol: "circle.dotted", title: "Animations",
                           normal: "Your position dot breathes",
                           saver: "Still: idle animations keep the screen redrawing when nothing moves."),
        BatterySaverChange(id: "screen", symbol: "sun.max.fill", title: "Screen",
                           normal: "Kept awake on a map (if Keep screen awake is on)",
                           saver: "Locks as usual. The biggest single saving: the display uses the most power."),
        BatterySaverChange(id: "network", symbol: "antenna.radiowaves.left.and.right", title: "Downloads",
                           normal: "Terrain fetched ahead of time; terrain around new maps kept at once",
                           saver: "Only what you look at; keeping the terrain around new maps waits until the saver is off."),
        BatterySaverChange(id: "terrain", symbol: "mountain.2.fill", title: "Terrain detail",
                           normal: "Finest (zoom 13)",
                           saver: "One step coarser (zoom 12) beyond the map's edge, on the world view and in 3D."),
        BatterySaverChange(id: "3d", symbol: "cube.fill", title: "3D view",
                           normal: "60 frames a second, smoothed edges",
                           saver: "30 frames a second, no edge smoothing."),
        BatterySaverChange(id: "recording", symbol: "record.circle", title: "Recording and routes",
                           normal: "Never paused; a track point every 4 m",
                           saver: "Never paused either: iOS may not pause location by itself, so tracks and route warnings keep going. In the background a track gets a point every 10 m."),
    ]

    /// The badge's short list.
    static var badgeSummary: [BatterySaverChange] {
        let short: [String: String] = [
            "accuracy": "GPS to about 10 m, fewer updates",
            "redraw": "Map redraw at most 30 times a second, no idle animations",
            "screen": "Screen locks as usual",
            "network": "No downloads ahead of time",
        ]
        return all.compactMap { change in
            short[change.id].map { BatterySaverChange(id: change.id, symbol: change.symbol, title: change.title,
                                                      normal: change.normal, saver: $0) }
        }
    }
}

/// Settings → Battery → What it changes: every change, normal against saver; what iOS does not
/// let an app do; how much it saves.
@MainActor
struct BatterySaverDetailsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        List {
            Section {
                ForEach(BatterySaverChange.all) { change in
                    VStack(alignment: .leading, spacing: 6) {
                        Label(change.title, systemImage: change.symbol)
                            .font(.body.weight(.semibold))
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text("Normal")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .frame(width: 52, alignment: .leading)
                            Text(change.normal)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text("Saver")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Theme.good)
                                .frame(width: 52, alignment: .leading)
                            Text(change.saver)
                                .font(.subheadline)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(.vertical, 4)
                    .accessibilityElement(children: .combine)
                }
            } header: {
                Text("What changes")
            } footer: {
                Text("Now: \(model.battery.statusText).")
            }
            Section {
                limitRow("Turning the GPS on and off between fixes", "iOS stopped letting apps defer or batch location updates in iOS 13.")
                limitRow("Dimming the screen", "An app's brightness only lasts while it is in front and iOS puts it back. MapMe leaves your brightness alone.")
                limitRow("Lowering the screen's refresh rate", "Only iOS does that; Low Power Mode already holds ProMotion displays at 60 Hz.")
            } header: {
                Text("What iOS doesn't let apps do")
            }
            Section {
                Text(BatterySaver.honestEstimate)
                    .font(.subheadline)
                    .fixedSize(horizontal: false, vertical: true)
                if let drain = model.battery.drainText {
                    LabeledContent("This session", value: drain)
                        .monospacedDigit()
                    Text("Your iPhone's whole battery use while MapMe ran, unplugged: iOS does not tell an app its own share.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                } else {
                    Text("MapMe shows how fast your battery drops after 15 minutes unplugged.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("How much it saves")
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle("Battery Saver")
        .navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier("batterySaverDetails")
    }

    private func limitRow(_ title: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.subheadline.weight(.semibold))
            Text(detail)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}
