import MapKit
import SwiftUI

/// A place found by the region picker's search: its name, where it is, the box to download.
struct RegionSearchResult: Identifiable, Hashable {
    let id = UUID()
    var name: String
    var detail: String?
    var bounds: GeoBounds
}

/// The region picker's place search: Apple Maps (`MKLocalSearch`: countries, states, provinces,
/// parks, places; physical features such as mountain ranges on iOS 18+), or the
/// `-mockRegionPlace` stand-in in UI tests. On the main actor, as MapKit's search objects are.
@MainActor
enum RegionPlaceSearch {
    /// Results shown at most.
    static let maxResults = 8
    /// The box around a place Apple Maps gives no extent for (a peak, a hut), metres either side.
    static let pointRadiusM = 10_000.0

    static func search(_ query: String, near region: MKCoordinateRegion?) async throws -> [RegionSearchResult] {
        if let mock = OfflineMocks.place {
            return [RegionSearchResult(name: mock.name, detail: "Test place", bounds: mock.bounds)]
        }
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = query
        var types: MKLocalSearch.ResultType = [.address, .pointOfInterest]
        if #available(iOS 18.0, *) { types.insert(.physicalFeature) }
        request.resultTypes = types
        request.pointOfInterestFilter = MKPointOfInterestFilter(including: [.nationalPark, .park, .campground, .beach])
        if let region { request.region = region }
        let response = try await MKLocalSearch(request: request).start()
        let items = Array(response.mapItems.prefix(maxResults))
        return items.map { item in
            let name = item.name ?? query
            let box: GeoBounds
            if items.count == 1 {
                // One answer: the response's region is that place's extent (a country, a park).
                box = Self.bounds(of: response.boundingRegion)
            } else if let circle = item.placemark.region as? CLCircularRegion {
                box = GeoBounds(south: circle.center.latitude, west: circle.center.longitude,
                                north: circle.center.latitude, east: circle.center.longitude)
                    .expanded(byMeters: max(circle.radius, 2_000))
            } else {
                let c = item.placemark.coordinate
                box = GeoBounds(south: c.latitude, west: c.longitude, north: c.latitude, east: c.longitude)
                    .expanded(byMeters: pointRadiusM)
            }
            let detail = [item.placemark.administrativeArea, item.placemark.country]
                .compactMap { $0 }
                .filter { $0 != name }
                .joined(separator: ", ")
            return RegionSearchResult(name: name, detail: detail.isEmpty ? nil : detail, bounds: box)
        }
    }

    static func bounds(of region: MKCoordinateRegion) -> GeoBounds {
        var west = region.center.longitude - region.span.longitudeDelta / 2
        var east = region.center.longitude + region.span.longitudeDelta / 2
        if west < -180 { west += 360 }
        if east > 180 { east -= 360 }
        return GeoBounds(south: max(-85, region.center.latitude - region.span.latitudeDelta / 2), west: west,
                         north: min(85, region.center.latitude + region.span.latitudeDelta / 2), east: east)
    }
}

/// Settings → Offline terrain → Add Region… (docs/design/explorer.md, Pick a region): search a
/// country, state, province, park or place, or start from "Around me" (50 km) or "Around my maps"
/// (all located maps + 30 km); adjust the rectangle's corners on the map; pick Standard or High
/// detail; see what each takes (tiles from the core's plan, bytes measured from sample tiles when
/// online: the level on screen first, then the other to compare) and download. Offline, the
/// search is off and the presets and the rectangle still work.
struct RegionPickerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var bounds: GeoBounds?
    @State private var name = ""
    /// The name was typed: a new search or preset no longer replaces it.
    @State private var nameEdited = false
    @State private var detail: OfflineDetail = .standard
    @State private var query = ""
    @State private var results: [RegionSearchResult] = []
    @State private var searching = false
    @State private var searchMessage: String?
    /// Both detail levels' estimates for the current box (missing: still working).
    @State private var estimates: [OfflineDetail: EstimateState] = [:]
    @State private var fitRequest = 0
    @State private var visible: GeoBounds?
    @State private var confirmCellular = false
    @State private var downloadTick = 0
    @FocusState private var searchFocused: Bool
    @FocusState private var nameFocused: Bool
    private let network = NetworkPathObserver.shared

    /// "Around me": metres either side of you.
    static let aroundMeM = 50_000.0
    /// "Around my maps": metres around all located maps.
    static let aroundMapsM = 30_000.0

    enum EstimateState: Equatable {
        case idle
        case working
        case ready(OfflineEstimate)
        case failed(String)
    }

    init() {}

    var body: some View {
        ZStack(alignment: .top) {
            RegionMapView(bounds: $bounds, fitRequest: fitRequest,
                          existing: model.offline.regions.map(\.bounds),
                          onVisibleRegion: { visible = $0 })
                .ignoresSafeArea()
            VStack(spacing: 10) {
                topBar
                if !results.isEmpty || searchMessage != nil {
                    resultsList
                } else {
                    presets
                }
                Spacer(minLength: 0)
                panel
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            .padding(.bottom, 8)
        }
        .background(Theme.background.ignoresSafeArea())
        .tint(Theme.accent)
        .task { applyInitialArea() }
        .task(id: estimateKey) { await refreshEstimate() }
        .sensoryFeedback(.success, trigger: downloadTick)
        .confirmationDialog("Download over mobile data?", isPresented: $confirmCellular, titleVisibility: .visible) {
            Button("Download Now") { startDownload(allowsCellular: true) }
            Button("Wait for Wi-Fi") { startDownload(allowsCellular: false) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(cellularMessage)
        }
        // Contain the children: an identifier on a plain container would replace theirs.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("regionPicker")
    }

    // MARK: - Top

    private var topBar: some View {
        HStack(spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .frame(width: Theme.minTapTarget, height: Theme.minTapTarget)
                    .floatingChrome(Circle(), interactive: true)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Cancel")
            .accessibilityIdentifier("cancelRegionPickerButton")

            HStack(spacing: 8) {
                Image(systemName: network.isOnline ? "magnifyingglass" : "wifi.slash")
                    .foregroundStyle(Theme.textSecondary)
                TextField(network.isOnline ? "Country, state, park or place" : "Search needs a connection", text: $query)
                    .textInputAutocapitalization(.words)
                    .autocorrectionDisabled()
                    .submitLabel(.search)
                    .focused($searchFocused)
                    .onSubmit { Task { await runSearch() } }
                    .disabled(!network.isOnline)
                    .accessibilityIdentifier("regionSearchField")
                if searching {
                    ProgressView()
                        .controlSize(.small)
                } else if !query.isEmpty {
                    Button {
                        query = ""
                        results = []
                        searchMessage = nil
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(Theme.textSecondary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear search")
                }
            }
            .padding(.horizontal, 14)
            .frame(height: Theme.minTapTarget)
            .floatingChrome(Capsule())
        }
    }

    private var presets: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                presetChip("Around Me", symbol: "location.fill", id: "presetAroundMe", enabled: here != nil) { aroundMe() }
                presetChip("Around My Maps", symbol: "map.fill", id: "presetAroundMaps", enabled: !locatedCorners.isEmpty) {
                    aroundMyMaps()
                }
                presetChip("This View", symbol: "viewfinder", id: "presetThisView", enabled: visible != nil) { thisView() }
            }
            .padding(.vertical, 2)
        }
    }

    private func presetChip(_ title: String, symbol: String, id: String, enabled: Bool,
                            action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(enabled ? Theme.textPrimary : Theme.textSecondary)
                .padding(.horizontal, 14)
                .frame(minHeight: 36)
                .floatingChrome(Capsule(), interactive: true)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.55)
        .frame(minHeight: Theme.minTapTarget)
        .accessibilityIdentifier(id)
    }

    private var resultsList: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let searchMessage, results.isEmpty {
                Text(searchMessage)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .padding(14)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            ForEach(Array(results.enumerated()), id: \.element.id) { index, result in
                Button {
                    pick(result)
                } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "mappin.circle.fill")
                            .font(.title3)
                            .foregroundStyle(Theme.accent)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(result.name)
                                .foregroundStyle(Theme.textPrimary)
                                .lineLimit(1)
                            if let detail = result.detail {
                                Text(detail)
                                    .font(.caption)
                                    .foregroundStyle(Theme.textSecondary)
                                    .lineLimit(1)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 14)
                    .frame(minHeight: Theme.minTapTarget + 8)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("regionResult-\(index)")
                if index < results.count - 1 {
                    Divider().padding(.leading, 50)
                }
            }
        }
        .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
    }

    // MARK: - Panel

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            if bounds == nil {
                Label("Search for a place, or start from a preset, then drag the corners.", systemImage: "hand.draw")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                TextField("Region name", text: $name)
                    .font(.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .focused($nameFocused)
                    .submitLabel(.done)
                    .onChange(of: nameFocused) { _, focused in
                        if focused { nameEdited = true }
                    }
                    .accessibilityIdentifier("regionNameField")
            }
            Picker("Detail", selection: $detail) {
                ForEach(OfflineDetail.allCases) { level in
                    Text(level.label).tag(level)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier("regionDetailPicker")
            Text(detail.summary)
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            estimateView
            if let comparison = comparisonLine {
                Text(comparison)
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .monospacedDigit()
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("regionOtherEstimateText")
            }
            Button {
                download()
            } label: {
                Label("Download", systemImage: "arrow.down.circle.fill")
                    .font(.headline)
                    .foregroundStyle(canDownload ? Color.black : Theme.textSecondary)
                    .frame(maxWidth: .infinity, minHeight: 50)
                    .background(canDownload ? Theme.accent : Theme.surfaceRaised,
                                in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(!canDownload)
            .accessibilityIdentifier("downloadRegionButton")
        }
        .padding(16)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
    }

    @ViewBuilder
    private var estimateView: some View {
        VStack(alignment: .leading, spacing: 4) {
            switch estimate {
            case .idle:
                Text(bounds == nil ? "No area yet" : "Working out the size\u{2026}")
                    .foregroundStyle(Theme.textSecondary)
                    .accessibilityIdentifier("regionEstimateText")
            case .working:
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text(network.isOnline ? "Measuring the download\u{2026}" : "Working out the size\u{2026}")
                        .foregroundStyle(Theme.textSecondary)
                }
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("regionEstimateText")
            case .ready(let e):
                Text(OfflineText.estimateLine(e))
                    .font(.body.weight(.semibold))
                    .foregroundStyle(e.refusal == nil ? Theme.textPrimary : Theme.bad)
                    .monospacedDigit()
                    .accessibilityIdentifier("regionEstimateText")
                if let refusal = e.refusal {
                    Label(refusal, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(Theme.bad)
                        .accessibilityIdentifier("regionRefusalText")
                } else {
                    Text(Self.footnote(e, online: network.isOnline, expensive: network.isExpensive))
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                        .monospacedDigit()
                }
            case .failed(let message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote)
                    .foregroundStyle(Theme.warn)
                    .accessibilityIdentifier("regionEstimateText")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The other detail level, to compare: "High detail: ≈ 1.2 GB · 9 443 tiles".
    private var comparisonLine: String? {
        guard bounds != nil else { return nil }
        let other: OfflineDetail = detail == .standard ? .high : .standard
        guard case .ready(let e)? = estimates[other] else { return nil }
        return Self.comparisonLine(e, detail: other)
    }

    static func comparisonLine(_ e: OfflineEstimate, detail: OfflineDetail) -> String {
        var line = "\(detail.label): \(OfflineText.approximateSize(bytes: e.bytes)) \u{00B7} \(OfflineText.tiles(e.plan.tiles))"
        if e.refusal != nil { line += " \u{00B7} too large to download" }
        return line
    }

    /// "Wi-Fi recommended · 48 GB free" / "Estimated offline: sizes vary 2–4× by region".
    static func footnote(_ e: OfflineEstimate, online: Bool, expensive: Bool) -> String {
        var parts: [String] = []
        if !e.isMeasured {
            parts.append(online ? "Estimated from typical tile sizes" : "Estimated offline: real sizes vary 2\u{2013}4\u{00D7}")
        }
        if e.bytes >= OfflinePlanner.wifiRecommendedBytes { parts.append(expensive ? "You\u{2019}re on mobile data" : "Wi-Fi recommended") }
        if let free = e.freeBytes { parts.append("\(OfflineText.size(bytes: free)) free") }
        return parts.joined(separator: " \u{00B7} ")
    }

    // MARK: - State

    private var here: LatLon? { model.location.location.map { LatLon($0.coordinate) } }

    private var locatedCorners: [LatLon] {
        model.library.maps.compactMap { OfflinePlanner.corners(of: $0) }.flatMap { $0 }
    }

    /// The estimate of the level on screen.
    private var estimate: EstimateState {
        guard bounds != nil else { return .idle }
        return estimates[detail] ?? .working
    }

    private var canDownload: Bool {
        guard bounds != nil, case .ready(let e) = estimate else { return false }
        return e.refusal == nil && e.plan.tiles > 0
    }

    /// Changes when the estimates must be redone (the box to ~100 m; both levels are worked out
    /// for every box, so switching the level does not).
    private var estimateKey: String {
        guard let b = bounds else { return "none" }
        return String(format: "%.3f,%.3f,%.3f,%.3f", b.south, b.west, b.north, b.east)
    }

    private var cellularMessage: String {
        if case .ready(let e) = estimate {
            return "This region is about \(OfflineText.size(bytes: e.bytes)). It can wait for Wi-Fi and start by itself."
        }
        return "It can wait for Wi-Fi and start by itself."
    }

    private func refreshEstimate() async {
        estimates = [:]
        guard let bounds else { return }
        // Dragging a corner: wait for a pause.
        try? await Task.sleep(nanoseconds: 450_000_000)
        if Task.isCancelled { return }
        // The level on screen first (online about a second each: a core plan and 16 HEAD
        // requests), then the other one, so the two sizes can be compared before downloading.
        let order = [detail] + OfflineDetail.allCases.filter { $0 != detail }
        for level in order {
            let result = await model.offline.makeEstimate(bounds, detail: level)
            if Task.isCancelled { return }
            switch result {
            case .success(let e): estimates[level] = .ready(e)
            case .failure(let failure): estimates[level] = .failed("Couldn\u{2019}t work out this area: \(failure.message)")
            }
        }
    }

    private func runSearch() async {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        searching = true
        searchMessage = nil
        defer { searching = false }
        do {
            let near = visible.map { v in
                MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: (v.south + v.north) / 2, longitude: (v.west + v.east) / 2),
                                   span: MKCoordinateSpan(latitudeDelta: max(v.north - v.south, 1), longitudeDelta: max(v.east - v.west, 1)))
            }
            let found = try await RegionPlaceSearch.search(text, near: near)
            results = found
            if found.isEmpty { searchMessage = "Nothing found for \u{201C}\(text)\u{201D}." }
        } catch {
            results = []
            searchMessage = network.isOnline ? "Apple Maps didn\u{2019}t find \u{201C}\(text)\u{201D}." : "Search needs a connection."
        }
    }

    private func pick(_ result: RegionSearchResult) {
        searchFocused = false
        results = []
        searchMessage = nil
        setArea(result.bounds, name: result.name)
    }

    private func applyInitialArea() {
        guard bounds == nil else { return }
        if here != nil {
            aroundMe()
        } else if !locatedCorners.isEmpty {
            aroundMyMaps()
        }
    }

    private func aroundMe() {
        guard let here else { return }
        let box = GeoBounds(south: here.lat, west: here.lon, north: here.lat, east: here.lon).expanded(byMeters: Self.aroundMeM)
        setArea(box, name: "Around me")
    }

    private func aroundMyMaps() {
        guard let box = GeoBounds.enclosing(locatedCorners)?.expanded(byMeters: Self.aroundMapsM) else { return }
        setArea(box, name: "Around my maps")
    }

    private func thisView() {
        guard let v = visible else { return }
        // A little inside the screen's edges, clear of the search bar and the panel.
        let dLat = (v.north - v.south) * 0.2
        let dLon = (v.east - v.west) * 0.12
        let box = GeoBounds(south: v.south + dLat * 1.4, west: v.west + dLon, north: v.north - dLat, east: v.east - dLon)
        guard box.north > box.south, box.east > box.west else { return }
        setArea(box, name: "Custom area", fit: false)
    }

    private func setArea(_ box: GeoBounds, name newName: String, fit: Bool = true) {
        bounds = box
        if !nameEdited { name = newName }
        if fit { fitRequest += 1 }
    }

    private func download() {
        let allowsCellular = model.settings.offlineDownloadsOnCellular
        if network.isExpensive, !allowsCellular {
            confirmCellular = true
            return
        }
        startDownload(allowsCellular: nil)
    }

    private func startDownload(allowsCellular: Bool?) {
        guard let bounds, case .ready(let e) = estimate, e.refusal == nil else { return }
        model.offline.download(name: name, bounds: bounds, detail: detail, estimate: e, allowsCellular: allowsCellular)
        downloadTick += 1
        dismiss()
    }
}
