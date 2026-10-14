import SwiftUI

/// "Search wider": how far to look for a map the first search did not find (100, 250, 500,
/// 1 000 km or a custom 10–2 000 km), whether to check the places named on the map first, and what
/// it will download. Start asks first when that is over 50 MB on mobile data.
///
/// The first few names on the map are looked up with Apple Maps as the sheet opens, so it can say
/// where they are ("Giant's Castle, 452 km away") and pick a distance that reaches them: a map of
/// somewhere far from you is found by checking those places, which the search does only within
/// the distance picked.
struct SearchWiderSheet: View {
    let mapName: String
    /// The radius already searched, km.
    let previousRadiusKm: Double?
    /// Names on the map from the last search (what "Use names on the map" looks up).
    let hintQueries: [String]
    /// Your position, if known.
    let position: LatLon?
    let units: DistanceUnits
    /// Looks up the names on the map (Apple Maps; a stand-in in tests).
    let geocoder: PlaceGeocoding
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
    /// Where Apple Maps found the first names on the map (`shownPlaces` measures them from you).
    @State private var places: [NamedPlace] = []
    @State private var lookingUpPlaces = false
    /// The user picked a distance: the places found later no longer change it.
    @State private var radiusPicked = false
    @FocusState private var customFocused: Bool
    private let network = NetworkPathObserver.shared

    init(mapName: String, previousRadiusKm: Double?, hintQueries: [String], position: LatLon?, units: DistanceUnits,
         geocoder: PlaceGeocoding = AppPlaceGeocoder.make(),
         onStart: @escaping (WideSearchChoice) -> Void, onCancel: @escaping () -> Void) {
        self.mapName = mapName
        self.previousRadiusKm = previousRadiusKm
        self.hintQueries = hintQueries
        self.position = position
        self.units = units
        self.geocoder = geocoder
        self.onStart = onStart
        self.onCancel = onCancel
        let preset = WideSearch.defaultPresetKm(after: previousRadiusKm, units: units)
        _selection = State(initialValue: preset.map { Selection.preset($0) } ?? .custom)
        _lookingUpPlaces = State(initialValue: !HintGeocoder.cleanQueries(hintQueries).isEmpty)
        _customText = State(initialValue: preset == nil ? "\(WideSearch.customRange(units: units).upperBound)" : "")
    }

    var body: some View {
        NavigationStack {
            Form {
                header
                if position == nil {
                    noPositionSection
                }
                if !lookupNames.isEmpty {
                    placesSection
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
        .task(id: lookupNames) { await lookUpPlaces() }
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

    /// Where the names on the map are: "Giant's Castle, KwaZulu-Natal, 452 km away".
    private var placesSection: some View {
        let shown = shownPlaces
        return Section {
            if shown.isEmpty {
                HStack(spacing: 10) {
                    if lookingUpPlaces {
                        ProgressView()
                    }
                    Text(placesStatusText)
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("namedPlacesStatus")
            } else {
                ForEach(Array(shown.enumerated()), id: \.element.id) { index, place in
                    placeRow(place)
                        .accessibilityIdentifier("namedPlace-\(index)")
                }
            }
        } header: {
            Text("Named on the map")
        } footer: {
            let text = Self.placesFooter(shown, selectedKm: position == nil ? nil : selectedKm,
                                         previousKm: previousRadiusKm, useMapNames: useMapNames, units: units)
            if !text.isEmpty {
                Text(text)
                    .accessibilityIdentifier("namedPlacesFooter")
            }
        }
    }

    private func placeRow(_ place: NamedPlace) -> some View {
        let reached = Self.reaches(selectedKm, place)
        return HStack(spacing: 12) {
            Image(systemName: "mappin.circle.fill")
                .font(.title3)
                .foregroundStyle(reached ? Theme.accent : Theme.textSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(place.name)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                if let area = place.area {
                    Text(area)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if let km = place.distanceKm {
                Text("\(SearchText.distance(km: km, units: units)) away")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(reached ? Theme.textPrimary : Theme.warn)
                    .monospacedDigit()
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
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
                    radiusPicked = true
                    selection = .preset(km)
                    customFocused = false
                }
                .accessibilityIdentifier("radiusOption-\(Self.displayValue(km, units: units))")
            }
            radiusRow(title: "Custom", km: selection == .custom ? customKm : nil, selected: selection == .custom) {
                radiusPicked = true
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
        .windowSafeAreaPadding(.bottom)
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

    /// The names looked up as the sheet opens (cleaned, the core's best first).
    private var lookupNames: [String] {
        Array(HintGeocoder.cleanQueries(hintQueries).prefix(WideSearch.namesLookedUpInSheet))
    }

    /// The places found, measured from where you are now.
    private var shownPlaces: [NamedPlace] {
        places.map { place in
            var measured = place
            measured.distanceKm = position.map { Geodesy.distance($0, place.coordinate) / 1000 }
            return measured
        }
    }

    private var placesStatusText: String {
        if lookingUpPlaces { return "Looking up the names in Apple Maps\u{2026}" }
        if !network.isOnline { return "Offline: the names on the map can\u{2019}t be looked up now." }
        return "Apple Maps didn\u{2019}t find the names on this map."
    }

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
        guard position != nil else { return "Not found yet. Look for the places named on the map?" }
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

    /// Looks up the first names on the map and, unless a distance was picked meanwhile, selects
    /// one that reaches the places found.
    private func lookUpPlaces() async {
        let names = lookupNames
        guard !names.isEmpty else { return }
        lookingUpPlaces = true
        let finder = HintGeocoder(geocoder: geocoder, isOnline: { NetworkPathObserver.isOnlineNow })
        let found = await finder.namedPlaces(for: names, near: position, limit: WideSearch.namesLookedUpInSheet)
        if Task.isCancelled { return }
        lookingUpPlaces = false
        withAnimation(.snappy) { places = found }
        guard !radiusPicked, position != nil,
              let km = WideSearch.suggestedRadiusKm(placeDistancesKm: found.compactMap(\.distanceKm),
                                                    after: previousRadiusKm, units: units)
        else { return }
        if let preset = WideSearch.presetsKm(units: units).first(where: { abs($0 - km) < 0.5 }) {
            selection = .preset(preset)
        } else {
            customText = "\(Self.displayValue(km, units: units))"
            selection = .custom
        }
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
    nonisolated static func displayValue(_ km: Double, units: DistanceUnits) -> Int {
        Int((units == .metric ? km : km / WideSearch.kmPerMile).rounded())
    }

    /// Whether a search out to `km` checks `place` (always, without a distance to it).
    nonisolated static func reaches(_ km: Double?, _ place: NamedPlace) -> Bool {
        guard let km, let distance = place.distanceKm else { return true }
        return distance <= km + 0.5
    }

    /// Under the places named on the map: whether the distance picked reaches them, and what is
    /// done with them.
    nonisolated static func placesFooter(_ places: [NamedPlace], selectedKm: Double?, previousKm: Double?,
                                         useMapNames: Bool, units: DistanceUnits) -> String {
        guard !places.isEmpty else { return "" }
        let checked = useMapNames
            ? "MapMe checks these places first."
            : "Turn on \u{201C}Use names on the map\u{201D} to check these places first."
        let distances = places.compactMap(\.distanceKm)
        guard !distances.isEmpty else { return checked }
        guard distances.contains(where: { $0 <= WideSearch.maxRadiusKm }) else {
            return "More than \(SearchText.distance(km: WideSearch.maxRadiusKm, units: units)) from you: too far to search."
        }
        guard let needed = WideSearch.suggestedRadiusKm(placeDistancesKm: distances, after: previousKm, units: units)
        else {
            let searched = SearchText.distance(km: previousKm ?? WideSearch.nearRadiusKm, units: units)
            return "Within the \(searched) searched already."
        }
        if let selectedKm, selectedKm + 0.5 < needed {
            return "Pick \(SearchText.distance(km: needed, units: units)) or more to reach them."
        }
        return checked
    }

    /// What "Use names on the map" will do, naming up to two of the names.
    nonisolated static func namesFooter(_ names: [String]) -> String {
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
