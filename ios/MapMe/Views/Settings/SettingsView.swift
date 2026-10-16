import CoreLocation
import SwiftUI
import UIKit

/// Preferences, saved tracks (GPX export), storage, about.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    init() {}

    var body: some View {
        NavigationStack {
            Form {
                SettingsDisplaySection()
                MapsSettingsSection()           // maps: auto-switching, terrain beyond the sheet
                RoutesSettingsSection()         // routes: pace, off-route alerts, daylight
                BatterySettingsSection()        // battery saver
                SettingsLocatingSection()
                OfflineSettingsSection()        // offline regions, terrain around your maps
                TracksSection()
                SettingsStorageSection()
                SettingsAboutSection()
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .fontWeight(.semibold)
                        .accessibilityIdentifier("settingsDone")
                }
            }
        }
        .tint(Theme.accent)
    }
}

/// iOS-Settings-style row label: a symbol on a tinted rounded square plus a title.
struct SettingsLabel: View {
    let title: String
    let symbol: String
    var color: Color = Theme.accent

    init(_ title: String, symbol: String, color: Color = Theme.accent) {
        self.title = title
        self.symbol = symbol
        self.color = color
    }

    var body: some View {
        Label {
            Text(title)
                .foregroundStyle(Theme.textPrimary)
        } icon: {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.white)
                .frame(width: 28, height: 28)
                .background(color.gradient, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
        }
    }
}

// MARK: - Display

private struct SettingsDisplaySection: View {
    @Environment(AppModel.self) private var model

    init() {}

    var body: some View {
        @Bindable var settings = model.settings
        Section {
            Picker(selection: $settings.mapAppearance) {
                ForEach(MapAppearance.allCases) { appearance in
                    Label(appearance.label, systemImage: appearance.symbolName).tag(appearance)
                }
            } label: {
                SettingsLabel("Map appearance", symbol: "circle.lefthalf.filled", color: .purple)
            }
            .accessibilityIdentifier("mapAppearancePicker")

            Picker(selection: $settings.units) {
                ForEach(DistanceUnits.allCases) { u in
                    Text(Self.unitsLabel(u)).tag(u)
                }
            } label: {
                SettingsLabel("Units", symbol: "ruler.fill", color: .orange)
            }

            Picker(selection: $settings.coordinateFormat) {
                ForEach(CoordinateFormat.allCases) { f in
                    Text(f.label).tag(f)
                }
            } label: {
                SettingsLabel("Coordinates", symbol: "globe", color: .blue)
            }

            Picker(selection: $settings.orientationMode) {
                ForEach(MapOrientationMode.selectable) { mode in
                    Label(mode.label, systemImage: mode.symbolName).tag(mode)
                }
            } label: {
                SettingsLabel("Map orientation", symbol: "location.north.line.fill", color: .indigo)
            }

            Toggle(isOn: $settings.allowMapRotation) {
                SettingsLabel("Allow map rotation", symbol: "rotate.right.fill", color: .teal)
            }

            LocationColorRow()

            Toggle(isOn: $settings.keepScreenOn) {
                SettingsLabel("Keep screen awake", symbol: "sun.max.fill", color: .yellow)
            }
            Toggle(isOn: $settings.showTrack) {
                SettingsLabel("Show trail", symbol: "point.topleft.down.curvedto.point.bottomright.up", color: Theme.track)
            }
            Toggle(isOn: $settings.showPOIs) {
                SettingsLabel("Show points of interest", symbol: "mappin.and.ellipse", color: .pink)
            }
        } header: {
            Text("Display")
        } footer: {
            VStack(alignment: .leading, spacing: 6) {
                Text("Dark redraws your map for the screen: dark paper, light ink, every colour kept. Paper shows it as printed.")
                SettingsCoordinatePreview()
            }
        }
    }

    static func unitsLabel(_ units: DistanceUnits) -> String {
        switch units {
        case .metric: return "Metric (km, m)"
        case .imperial: return "Imperial (mi, ft)"
        }
    }
}

/// Red or blue position puck, picked from two swatches.
private struct LocationColorRow: View {
    @Environment(AppModel.self) private var model

    init() {}

    var body: some View {
        let selected = model.settings.locationColor
        HStack {
            SettingsLabel("Location color", symbol: "location.fill", color: Color(uiColor: selected.uiColor))
            Spacer()
            HStack(spacing: 12) {
                ForEach(LocationColor.allCases) { option in
                    let on = option == selected
                    Button {
                        withAnimation(.snappy) { model.settings.locationColor = option }
                    } label: {
                        ZStack {
                            Circle()
                                .fill(Color.white)
                                .frame(width: 26, height: 26)
                            Circle()
                                .fill(Color(uiColor: option.uiColor).gradient)
                                .frame(width: 19, height: 19)
                        }
                        .padding(3)
                        .overlay(Circle().stroke(on ? Color(uiColor: option.uiColor) : Color.clear, lineWidth: 2))
                        .shadow(color: Color.black.opacity(0.25), radius: 2, y: 1)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(option.label)
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
        }
        .sensoryFeedback(.selection, trigger: selected)
    }
}

/// "Your position: …" in the chosen coordinate format.
private struct SettingsCoordinatePreview: View {
    @Environment(AppModel.self) private var model

    init() {}

    var body: some View {
        if let fix = model.location.location {
            Text("Your position: " + GeoFormat.coordinate(LatLon(fix.coordinate), format: model.settings.coordinateFormat))
                .monospacedDigit()
        } else {
            Text("Heading up opens maps following your heading, so the map turns as you turn.")
        }
    }
}

// MARK: - Locating

private struct SettingsLocatingSection: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL

    init() {}

    var body: some View {
        @Bindable var settings = model.settings
        let range = AppSettings.searchRadiusRangeKm
        Section {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    SettingsLabel("Terrain search radius", symbol: "scope", color: .teal)
                    Spacer()
                    Text(Self.radiusText(settings.searchRadiusKm, units: settings.units))
                        .font(.body.weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(Theme.accent)
                        .contentTransition(.numericText())
                        .animation(.snappy, value: settings.searchRadiusKm)
                }
                Slider(value: $settings.searchRadiusKm, in: range, step: 5) {
                    Text("Terrain search radius")
                } minimumValueLabel: {
                    Text(Self.radiusText(range.lowerBound, units: settings.units)).font(.caption2).foregroundStyle(Theme.textSecondary)
                } maximumValueLabel: {
                    Text(Self.radiusText(range.upperBound, units: settings.units)).font(.caption2).foregroundStyle(Theme.textSecondary)
                }
                .accessibilityIdentifier("searchRadiusSlider")
            }
            .padding(.vertical, 4)

            Toggle(isOn: $settings.useOpenStreetMap) {
                SettingsLabel("OpenStreetMap places", symbol: "map.fill", color: .green)
            }

            locationAccessRow

            NavigationLink {
                LocatingExplainerView()
            } label: {
                SettingsLabel("How MapMe finds your map", symbol: "wand.and.stars", color: Theme.accent.opacity(0.8))
            }
        } header: {
            Text("Locating")
        } footer: {
            Text("When a map has no coordinates printed on it, MapMe first matches its contour lines against real terrain within this distance of you. If the map isn't found, you can search wider, up to 2\u{00A0}000 km, from the import screen or the map's menu in the library.")
        }
    }

    private enum Access {
        case granted(String), undetermined, denied
    }

    @MainActor
    private var access: Access {
        switch model.location.authorization {
        case .authorizedAlways: return .granted("Always")
        case .authorizedWhenInUse: return .granted("While using")
        case .notDetermined: return .undetermined
        case .denied, .restricted: return .denied
        @unknown default: return .denied
        }
    }

    @MainActor
    private var locationAccessRow: some View {
        HStack {
            SettingsLabel("Location access", symbol: "location.fill", color: Theme.userRed)
            Spacer()
            switch access {
            case .granted(let text):
                Text(text)
                    .foregroundStyle(Theme.textSecondary)
            case .undetermined:
                Button("Allow") { model.location.requestPermission() }
                    .buttonStyle(.borderless)
            case .denied:
                Button("Open Settings") {
                    if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                }
                .buttonStyle(.borderless)
                .foregroundStyle(Theme.warn)
            }
        }
    }

    static func radiusText(_ km: Double, units: DistanceUnits) -> String {
        switch units {
        case .metric: return "\(Int(km.rounded())) km"
        case .imperial: return "\(Int((km / 1.609344).rounded())) mi"
        }
    }
}

/// Plain-language explanation of the georeferencing pipeline.
private struct LocatingExplainerView: View {
    private struct Step: Identifiable {
        let id: Int
        let symbol: String
        let color: Color
        let title: String
        let text: String
    }

    init() {}

    private let steps: [Step] = [
        Step(id: 1, symbol: "number", color: Theme.good, title: "Coordinates printed on the map",
             text: "MapMe reads the latitude/longitude or UTM labels around the border and finds the grid lines they belong to. This is the most accurate method, usually within a few metres."),
        Step(id: 2, symbol: "mountain.2.fill", color: .orange, title: "Contour lines vs real terrain",
             text: "No coordinates? MapMe traces the map's contour lines and compares their shape with satellite elevation data around you. The pattern of hills and valleys is like a fingerprint: it finds where it fits, then fine-tunes so every contour line sits at one height."),
        Step(id: 3, symbol: "hand.tap.fill", color: Theme.accent, title: "Calibrate by hand",
             text: "If both fail, tap a few spots on your map and the same spots on the satellite map, or tap where you are and press \u{201C}I\u{2019}m here\u{201D}. Two points work; three or more spread far apart are best."),
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("MapMe works out where on Earth your map is, so it can put your GPS position on it. It tries these in order:")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)

                ForEach(steps) { step in
                    HStack(alignment: .top, spacing: 14) {
                        ZStack(alignment: .topTrailing) {
                            SymbolBadge(symbol: step.symbol, color: step.color, size: 44)
                            Text("\(step.id)")
                                .font(.caption2.weight(.heavy))
                                .foregroundStyle(Color.black)
                                .frame(width: 18, height: 18)
                                .background(step.color, in: Circle())
                                .offset(x: 4, y: -4)
                        }
                        VStack(alignment: .leading, spacing: 6) {
                            Text(step.title)
                                .font(.headline)
                                .foregroundStyle(Theme.textPrimary)
                            Text(step.text)
                                .font(.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(14)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.corner, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
                }

                VStack(alignment: .leading, spacing: 8) {
                    Label("Tips", systemImage: "lightbulb.fill")
                        .font(.headline)
                        .foregroundStyle(Theme.warn)
                    Text("\u{2022} Import the map while you are on or near it, so contour matching knows where to look.")
                    Text("\u{2022} Map from far away? When it isn't found near you, Search wider looks up to 2\u{00A0}000 km away, starting with the places named on the map.")
                    Text("\u{2022} Unusual contour colours? Open Calibrate, pick the contour colour, and locate again.")
                    Text("\u{2022} Photographing a paper map? Lay it flat, fill the frame and include the whole page so its corners can be found.")
                }
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .glass()
            }
            .padding(16)
        }
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle("How it works")
        .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - Storage

private struct SettingsStorageSection: View {
    @Environment(AppModel.self) private var model
    @State private var mapsBytes: Int64? = nil
    @State private var demBytes: Int64? = nil
    @State private var clearing = false
    @State private var refreshTick = 0

    init() {}

    var body: some View {
        let mapCount = model.library.maps.count
        Section {
            LabeledContent {
                Text("\(mapCount)")
                    .monospacedDigit()
            } label: {
                SettingsLabel("Maps", symbol: "map.fill", color: .blue)
            }
            .task(id: "\(mapCount)-\(refreshTick)") {
                await measure()
            }
            LabeledContent {
                sizeText(mapsBytes)
            } label: {
                SettingsLabel("Map storage", symbol: "internaldrive.fill", color: .gray)
            }
            LabeledContent {
                sizeText(demBytes)
            } label: {
                SettingsLabel("Terrain cache", symbol: "mountain.2.fill", color: .brown)
            }
            Button(role: .destructive) {
                clearTerrainCache()
            } label: {
                HStack {
                    Label("Clear terrain cache", systemImage: "trash")
                    if clearing {
                        Spacer()
                        ProgressView()
                    }
                }
            }
            .disabled(clearing || demBytes == 0)
        } header: {
            Text("Storage")
        } footer: {
            Text("Terrain tiles are downloaded when a map is located from its contour lines and kept up to 400\u{00A0}MB (the least recently used go first). They are downloaded again when needed.")
        }
    }

    private func sizeText(_ bytes: Int64?) -> some View {
        Group {
            if let bytes {
                Text(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file))
                    .monospacedDigit()
            } else {
                ProgressView()
                    .controlSize(.small)
            }
        }
    }

    @MainActor
    private func measure() async {
        let mapsURL = model.library.rootURL
        let demURL = DEMService.shared.cacheDirectory
        let sizes = await Task.detached(priority: .utility) { () -> (Int64, Int64) in
            (StorageUsage.directorySize(mapsURL), StorageUsage.directorySize(demURL))
        }.value
        mapsBytes = sizes.0
        demBytes = sizes.1
    }

    @MainActor
    private func clearTerrainCache() {
        clearing = true
        let dem = DEMService.shared
        Task { @MainActor in
            await Task.detached(priority: .userInitiated) {
                dem.clearCache()
            }.value
            clearing = false
            refreshTick += 1
        }
    }
}

enum StorageUsage {
    /// Allocated size of all regular files below `url` (0 if it does not exist). Blocking.
    static func directorySize(_ url: URL) -> Int64 {
        let keys: [URLResourceKey] = [.isRegularFileKey, .totalFileAllocatedSizeKey, .fileAllocatedSizeKey, .fileSizeKey]
        guard let enumerator = FileManager.default.enumerator(at: url, includingPropertiesForKeys: keys) else { return 0 }
        let keySet = Set(keys)
        var total: Int64 = 0
        while let item = enumerator.nextObject() {
            guard let file = item as? URL,
                  let values = try? file.resourceValues(forKeys: keySet),
                  values.isRegularFile == true
            else { continue }
            total += Int64(values.totalFileAllocatedSize ?? values.fileAllocatedSize ?? values.fileSize ?? 0)
        }
        return total
    }
}

// MARK: - About

private struct SettingsAboutSection: View {
    private static let appVersion: String = {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(version) (\(build))"
    }()

    @State private var coreVersion = ""

    init() {}

    var body: some View {
        Section {
            HStack(spacing: 14) {
                ZStack {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .fill(LinearGradient(colors: [Theme.surfaceRaised, Theme.background],
                                             startPoint: .topLeading, endPoint: .bottomTrailing))
                    Image(systemName: "location.fill")
                        .font(.system(size: 20, weight: .bold))
                        .foregroundStyle(Theme.userRed)
                        .shadow(color: Theme.userRed.opacity(0.6), radius: 6)
                }
                .frame(width: 48, height: 48)
                .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
                VStack(alignment: .leading, spacing: 2) {
                    Text("MapMe")
                        .font(.system(.title3, design: .rounded).weight(.bold))
                        .foregroundStyle(Theme.textPrimary)
                    Text("Your position on any map.")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            .padding(.vertical, 4)
            .task {
                if coreVersion.isEmpty {
                    coreVersion = MapMeCore.version
                }
            }

            LabeledContent("Version", value: Self.appVersion)
                .monospacedDigit()
            LabeledContent("Locating engine", value: coreVersion.isEmpty ? "…" : "mapme-core \(coreVersion)")
                .monospacedDigit()

            VStack(alignment: .leading, spacing: 8) {
                creditRow(symbol: "mountain.2.fill", text: "Elevation: Terrain Tiles on AWS Open Data (SRTM, etc.)")
                creditRow(symbol: "mappin.and.ellipse", text: "Points of interest \u{00A9} OpenStreetMap contributors")
                creditRow(symbol: "globe.europe.africa.fill", text: "Satellite imagery: Apple Maps")
            }
            .padding(.vertical, 4)

            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "lock.shield.fill")
                    .foregroundStyle(Theme.good)
                Text("Your maps, tracks and position stay on this device. The only network requests are anonymous downloads of terrain tiles and OpenStreetMap places for the area of a map, and, when you search wider, Apple Maps lookups of the place names printed on the map.")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 4)
        } header: {
            Text("About")
        }
    }

    private func creditRow(symbol: String, text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: symbol)
                .font(.footnote)
                .foregroundStyle(Theme.accent)
                .frame(width: 18)
            Text(text)
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
