import SwiftUI

/// "Search wider": how far to look for a map the first search did not find (100, 250, 500,
/// 1 000 km or a custom 10–2 000 km), whether to check the places named on the map first, and what
/// it will download. Start asks first when that is over 50 MB on mobile data.
struct SearchWiderSheet: View {
    let mapName: String
    /// The radius already searched, km.
    let previousRadiusKm: Double?
    /// Names on the map from the last search (what "Use names on the map" looks up).
    let hintQueries: [String]
    /// Your position, if known.
    let position: LatLon?
    let units: DistanceUnits
    var onStart: (WideSearchChoice) -> Void
    var onCancel: () -> Void

    private enum Selection: Hashable {
        case preset(Double)
        case custom
    }

    @State private var selection: Selection
    @State private var customText: String
    @State private var useMapNames = true
    /// `mm_search_estimate` per radius in km (the presets and a valid custom value).
    @State private var estimates: [Double: SearchEstimate] = [:]
    @State private var confirmCellular = false
    @FocusState private var customFocused: Bool
    private let network = NetworkPathObserver.shared

    init(mapName: String, previousRadiusKm: Double?, hintQueries: [String], position: LatLon?, units: DistanceUnits,
         onStart: @escaping (WideSearchChoice) -> Void, onCancel: @escaping () -> Void) {
        self.mapName = mapName
        self.previousRadiusKm = previousRadiusKm
        self.hintQueries = hintQueries
        self.position = position
        self.units = units
        self.onStart = onStart
        self.onCancel = onCancel
        let preset = WideSearch.defaultPresetKm(after: previousRadiusKm, units: units)
        _selection = State(initialValue: preset.map { Selection.preset($0) } ?? .custom)
        _customText = State(initialValue: preset == nil ? "\(WideSearch.customRange(units: units).upperBound)" : "")
    }

    var body: some View {
        NavigationStack {
            Form {
                header
                if position == nil {
                    noPositionSection
                }
                distanceSection
                namesSection
            }
            .scrollContentBackground(.hidden)
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle("Search wider")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { onCancel() }
                        .accessibilityIdentifier("cancelSearchWiderButton")
                }
            }
            .safeAreaInset(edge: .bottom) { startBar }
        }
        .tint(Theme.accent)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .searchWiderSheetBackground()
        .task(id: estimateKey) { await refreshEstimates() }
        .sensoryFeedback(.selection, trigger: selection)
        .confirmationDialog("Use mobile data?", isPresented: $confirmCellular, titleVisibility: .visible) {
            Button("Search anyway") { commit() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(cellularMessage)
        }
    }

    // MARK: - Sections

    private var header: some View {
        Section {
            HStack(spacing: 16) {
                ImportScanVisual(phase: .locating, searchedFraction: 0.18)
                    .frame(width: 72, height: 72)
                VStack(alignment: .leading, spacing: 4) {
                    Text(mapName)
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(2)
                    Text(notFoundText)
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.vertical, 4)
            .listRowBackground(Color.clear)
        }
    }

    private var noPositionSection: some View {
        Section {
            Label {
                Text("Your position isn't known yet, so MapMe can only check the places named on the map.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            } icon: {
                Image(systemName: "location.slash.fill")
                    .foregroundStyle(Theme.warn)
            }
        }
    }

    private var distanceSection: some View {
        Section {
            ForEach(WideSearch.presetsKm(units: units), id: \.self) { km in
                radiusRow(title: SearchText.distance(km: km, units: units), km: km, selected: selection == .preset(km)) {
                    selection = .preset(km)
                    customFocused = false
                }
                .accessibilityIdentifier("radiusOption-\(Self.displayValue(km, units: units))")
            }
            radiusRow(title: "Custom", km: selection == .custom ? customKm : nil, selected: selection == .custom) {
                selection = .custom
                customFocused = true
            }
            .accessibilityIdentifier("radiusOptionCustom")
            if selection == .custom {
                HStack {
                    TextField("Distance", text: $customText)
                        .keyboardType(.numberPad)
                        .focused($customFocused)
                        .monospacedDigit()
                        .accessibilityIdentifier("customRadiusField")
                    Text(units == .metric ? "km" : "mi")
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        } header: {
            Text("How far from you")
        } footer: {
            VStack(alignment: .leading, spacing: 6) {
                Text(estimateText)
                    .accessibilityIdentifier("searchEstimateText")
                if network.isExpensive {
                    Label("You're on mobile data.", systemImage: "antenna.radiowaves.left.and.right")
                }
            }
        }
        .disabled(position == nil)
    }

    private var namesSection: some View {
        Section {
            Toggle(isOn: $useMapNames) {
                SettingsLabel("Use names on the map", symbol: "text.magnifyingglass", color: .orange)
            }
            .accessibilityIdentifier("useMapNamesToggle")
        } footer: {
            Text(Self.namesFooter(hintQueries))
        }
    }

    private var startBar: some View {
        Button {
            start()
        } label: {
            Label("Start search", systemImage: "location.magnifyingglass")
                .frame(maxWidth: .infinity)
        }
        .searchWiderStartButtonStyle()
        .disabled(!canStart)
        .opacity(canStart ? 1 : 0.45)
        .accessibilityIdentifier("startWideSearchButton")
        .padding(.horizontal, 20)
        .padding(.top, 8)
        .padding(.bottom, 10)
    }

    private func radiusRow(title: String, km: Double?, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Text(title)
                    .foregroundStyle(Theme.textPrimary)
                    .monospacedDigit()
                Spacer()
                if let km, let estimate = estimates[km] {
                    Text(SearchText.rowDetail(estimate))
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .monospacedDigit()
                }
                Image(systemName: "checkmark")
                    .font(.body.weight(.semibold))
                    .foregroundStyle(Theme.accent)
                    .opacity(selected ? 1 : 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    // MARK: - State

    private var customKm: Double? {
        let digits = customText.filter(\.isNumber)
        return WideSearch.customKm(Double(digits), units: units)
    }

    /// The radius Start would search, km (nil: an invalid custom value).
    private var selectedKm: Double? {
        switch selection {
        case .preset(let km): return km
        case .custom: return customKm
        }
    }

    private var canStart: Bool {
        position == nil ? useMapNames : selectedKm != nil
    }

    private var notFoundText: String {
        let radius = SearchText.distance(km: previousRadiusKm ?? WideSearch.nearRadiusKm, units: units)
        return "Not found within \(radius) of you. Look farther away?"
    }

    private var estimateText: String {
        if position == nil {
            return "Without your position only the places named on the map are checked."
        }
        guard let km = selectedKm else {
            let range = WideSearch.customRange(units: units)
            let unit = units == .metric ? "km" : "mi"
            return "Enter \(SearchText.grouped(range.lowerBound))–\(SearchText.grouped(range.upperBound)) \(unit)."
        }
        guard let estimate = estimates[km] else { return "Working out how much terrain data that is…" }
        return SearchText.estimateLine(estimate)
    }

    private var cellularMessage: String {
        guard let km = selectedKm, let estimate = estimates[km] else { return "This search downloads terrain data." }
        let size = SearchText.size(bytes: estimate.approxBytes)
        return "This search may download \(size.prefix(1).lowercased() + size.dropFirst()) of terrain data over mobile data."
    }

    /// Changes when the estimates must be redone (position to ~1 km, so a walk does not redo
    /// them every second).
    private var estimateKey: String {
        let p = position.map { String(format: "%.2f,%.2f", $0.lat, $0.lon) } ?? "none"
        return "\(p)|\(customKm ?? -1)|\(units.rawValue)"
    }

    private func refreshEstimates() async {
        guard let position else {
            estimates = [:]
            return
        }
        if selection == .custom {
            // Typing: wait for a pause.
            try? await Task.sleep(nanoseconds: 250_000_000)
            if Task.isCancelled { return }
        }
        let radii = WideSearch.presetsKm(units: units) + [customKm].compactMap { $0 }
        let found = await Task.detached(priority: .userInitiated) { () -> [Double: SearchEstimate] in
            var out: [Double: SearchEstimate] = [:]
            for km in radii {
                if let e = MapMeCore.searchEstimate(radiusKm: km, gps: position) { out[km] = e }
            }
            return out
        }.value
        if Task.isCancelled { return }
        estimates = found
    }

    private func start() {
        let estimate = selectedKm.flatMap { estimates[$0] }
        if SearchText.needsCellularConfirmation(bytes: estimate?.approxBytes, isExpensive: network.isExpensive) {
            confirmCellular = true
            return
        }
        commit()
    }

    private func commit() {
        let km = selectedKm ?? (position == nil ? WideSearch.presetsKm(units: units).last : nil)
        guard let km else { return }
        customFocused = false
        onStart(WideSearchChoice(radiusKm: km, useMapNames: useMapNames, estimate: estimates[km]))
    }

    // MARK: - Wording

    /// A preset's number in the user's units (accessibility identifiers: "radiusOption-1000").
    static func displayValue(_ km: Double, units: DistanceUnits) -> Int {
        Int((units == .metric ? km : km / WideSearch.kmPerMile).rounded())
    }

    /// What "Use names on the map" will do, naming up to two of the names.
    static func namesFooter(_ names: [String]) -> String {
        let cleaned = Array(HintGeocoder.cleanQueries(names).prefix(HintGeocoder.defaultMaxQueries))
        let quoted = cleaned.prefix(2).map { "\u{201C}\($0)\u{201D}" }
        let list: String
        switch cleaned.count {
        case 0:
            return "Looks up the places named on the map with Apple Maps and checks there first."
        case 1:
            list = quoted[0]
        case 2:
            list = "\(quoted[0]) and \(quoted[1])"
        default:
            list = "\(quoted[0]), \(quoted[1]) and \(cleaned.count - 2) more"
        }
        return "Looks up \(list) with Apple Maps and checks those places first."
    }
}

extension View {
    /// Liquid Glass on iOS 26 (the system sheet); the app's dark background before.
    @ViewBuilder
    func searchWiderSheetBackground() -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            self
        } else {
            self.presentationBackground(Theme.background)
        }
        #else
        self.presentationBackground(Theme.background)
        #endif
    }

    /// The sheet's Start button: prominent Liquid Glass tinted with the accent on iOS 26, the
    /// import screen's accent capsule before.
    @ViewBuilder
    func searchWiderStartButtonStyle() -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            self.buttonStyle(.glassProminent)
                .controlSize(.large)
                .font(.headline)
                .tint(Theme.accent)
        } else {
            self.buttonStyle(ImportActionButtonStyle(kind: .primary))
        }
        #else
        self.buttonStyle(ImportActionButtonStyle(kind: .primary))
        #endif
    }
}
