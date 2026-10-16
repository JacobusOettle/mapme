import CoreLocation
import SwiftUI

/// All your maps on Earth (docs/design/explorer.md, World view): their footprints with the map
/// drawn in, your routes, waypoints, notes and track, the terrain (also offline), and you; tap a
/// map to open it. Pushed from the library (globe button) and from "Maps here"; from the
/// walked-off banner it follows you. Maps not located yet are listed separately.
@MainActor
struct WorldView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var selectedMapID: UUID?
    @State private var focus: WorldFocus?
    @State private var followsUser = false
    @State private var listExpanded = false
    @State private var didSetUp = false
    @AppStorage("worldShowsTerrain") private var showsTerrain = true
    @AppStorage("worldShowsRegions") private var showsRegions = true
    @AppStorage("worldShowsItems") private var showsItems = true

    var body: some View {
        let located = model.library.maps.filter(\.isGeoreferenced)
        let unlocated = model.library.maps.filter { !$0.isGeoreferenced }
        let fix = model.location.location
        SafeAreaReader { _, safe in
            ZStack {
                WorldMapView(model: model, state: mapState(located), user: fix, focus: focus,
                             followsUser: $followsUser, onSelectMap: { id in select(id) })
                    .ignoresSafeArea()

                VStack(spacing: 0) {
                    topBar(located: located, unlocated: unlocated)
                        .padding(.top, safe.top + 4)
                        .padding(.leading, safe.leading + 12)
                        .padding(.trailing, safe.trailing + 12)
                    Spacer(minLength: 0)
                    bottom(located: located, unlocated: unlocated, here: fix)
                        .frame(maxWidth: 520)
                        .padding(.leading, safe.leading + 8)
                        .padding(.trailing, safe.trailing + 8)
                        .padding(.bottom, max(8, safe.bottom - 16) + 8)
                }
                .ignoresSafeArea()
            }
        }
        .background(Theme.mapBackground.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("worldView")
        .onAppear {
            guard !didSetUp else { return }
            didSetUp = true
            followsUser = model.mapSwitcher.worldViewFollowsUser
            model.mapSwitcher.worldViewFollowsUser = false
            if located.isEmpty { listExpanded = true }
        }
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: selectedMapID)
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: listExpanded)
    }

    // MARK: - Map state

    private func mapState(_ located: [MapDocument]) -> WorldMapState {
        var maps = Hasher()
        for map in located {
            maps.combine(map.id)
            maps.combine(map.name)
            maps.combine(map.georef)
            maps.combine(map.pixelWidth)
            maps.combine(map.pixelHeight)
        }
        var track = Hasher()
        track.combine(model.tracks.current.points.count)
        track.combine(model.tracks.current.points.last)
        var regions = Hasher()
        for region in model.offline.regions {
            regions.combine(region.id)
            regions.combine(region.bounds)
        }
        var pois = Hasher()
        for map in located {
            pois.combine(map.id)
            pois.combine(map.pois.count)
            pois.combine(map.pois.last?.id)
        }
        return WorldMapState(mapsKey: maps.finalize(), dark: model.settings.mapAppearance == .dark,
                             itemsKey: model.geoItems.revision, trackKey: track.finalize(),
                             regionsKey: regions.finalize(), poisKey: pois.finalize(),
                             showsPOIs: model.settings.showPOIs, showsTerrain: showsTerrain,
                             showsRegions: showsRegions, showsItems: showsItems,
                             terrainZoomCap: model.battery.maxTerrainZoom, selectedMapID: selectedMapID)
    }

    private func select(_ id: UUID?) {
        selectedMapID = id
        if id != nil { listExpanded = false }
    }

    /// Opens a map: back to its screen when the world view was opened from it, else pushed.
    private func open(_ id: UUID) {
        let path = model.path
        if path.count >= 2, path[path.count - 2] == .map(id), path.last == .world {
            dismiss()
        } else {
            model.path.append(.map(id))
        }
    }

    // MARK: - Chrome

    private func topBar(located: [MapDocument], unlocated: [MapDocument]) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left")
            }
            .buttonStyle(MapRoundButtonStyle())
            .accessibilityLabel("Back")
            .accessibilityIdentifier("worldBackButton")

            VStack(alignment: .leading, spacing: 1) {
                Text("Your maps")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.primary)
                Text(summary(located: located, unlocated: unlocated))
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(Color.secondary)
                    .lineLimit(1)
            }
            .padding(.leading, 16)
            .padding(.trailing, 18)
            .frame(minHeight: 44)
            .floatingChrome(Capsule())
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("worldTitle")

            Spacer(minLength: 0)

            VStack(spacing: 0) {
                WorldLayersMenu(showsTerrain: $showsTerrain, showsRegions: $showsRegions, showsItems: $showsItems,
                                hasRegions: !model.offline.regions.isEmpty)
                Rectangle()
                    .fill(Color.primary.opacity(0.16))
                    .frame(width: 28, height: 0.5)
                Button {
                    followsUser.toggle()
                    if followsUser { focus = WorldFocus(target: .user) }
                } label: {
                    Image(systemName: followsUser ? "location.fill" : "location")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(followsUser ? Color.blue : Color.primary)
                        .contentTransition(.symbolEffect(.replace))
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(PressScaleButtonStyle(scale: 0.88))
                .opacity(model.location.location == nil ? 0.55 : 1)
                .accessibilityLabel(followsUser ? "Following your position" : "Show my position")
                .accessibilityIdentifier("worldFollowButton")
            }
            .frame(width: 44)
            .floatingChrome(RoundedRectangle(cornerRadius: 22, style: .continuous))
            .sensoryFeedback(.selection, trigger: followsUser)
        }
    }

    private func summary(located: [MapDocument], unlocated: [MapDocument]) -> String {
        var parts = ["\(located.count) on the map"]
        if !unlocated.isEmpty { parts.append("\(unlocated.count) not located") }
        if showsTerrain { parts.append("terrain") }
        return parts.joined(separator: " · ")
    }

    @ViewBuilder
    private func bottom(located: [MapDocument], unlocated: [MapDocument], here: CLLocation?) -> some View {
        if let id = selectedMapID, let map = located.first(where: { $0.id == id }) {
            WorldMapCard(map: map, here: here.map { LatLon($0.coordinate) }, units: model.settings.units,
                         thumbnailURL: model.settings.mapAppearance == .dark
                            ? model.library.darkThumbnailURL(for: map) : model.library.thumbnailURL(for: map),
                         onOpen: { open(map.id) },
                         onClose: { selectedMapID = nil })
                .transition(.move(edge: .bottom).combined(with: .opacity))
        } else {
            WorldMapsList(located: located, unlocated: unlocated, here: here.map { LatLon($0.coordinate) },
                          units: model.settings.units, expanded: $listExpanded,
                          onSelect: { id in
                              select(id)
                              focus = WorldFocus(target: .map(id))
                          },
                          onOpen: { id in open(id) },
                          onShowAll: { focus = WorldFocus(target: .everything) })
                .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// Terrain, offline regions, routes and notes on or off.
private struct WorldLayersMenu: View {
    @Binding var showsTerrain: Bool
    @Binding var showsRegions: Bool
    @Binding var showsItems: Bool
    var hasRegions: Bool

    var body: some View {
        Menu {
            Toggle(isOn: $showsTerrain) {
                Label("Terrain Contours", systemImage: "mountain.2")
            }
            Toggle(isOn: $showsItems) {
                Label("Routes, Waypoints and Notes", systemImage: "point.topleft.down.curvedto.point.bottomright.up")
            }
            Toggle(isOn: $showsRegions) {
                Label(hasRegions ? "Offline Regions" : "Offline Regions (none yet)", systemImage: "square.dashed")
            }
        } label: {
            Image(systemName: "square.2.layers.3d")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Color.primary)
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .accessibilityLabel("World view layers")
        .accessibilityIdentifier("worldLayersButton")
    }
}

/// The selected map: thumbnail, name, how it was located, its scale and where you are; Open.
private struct WorldMapCard: View {
    let map: MapDocument
    let here: LatLon?
    let units: DistanceUnits
    let thumbnailURL: URL
    var onOpen: () -> Void
    var onClose: () -> Void
    @State private var thumbnail: UIImage?

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                Theme.surfaceRaised
                if let thumbnail {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .scaledToFill()
                } else {
                    Image(systemName: "map")
                        .font(.system(size: 22, weight: .light))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            .frame(width: 76, height: 76)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Theme.stroke, lineWidth: 1))

            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .top) {
                    Text(map.name)
                        .font(.headline)
                        .foregroundStyle(Color.primary)
                        .lineLimit(2)
                    Spacer(minLength: 4)
                    Button(action: onClose) {
                        Image(systemName: "xmark")
                            .font(.system(size: 12, weight: .bold))
                            .foregroundStyle(Color.secondary)
                            .frame(width: 30, height: 30)
                            .background(Color.primary.opacity(0.08), in: Circle())
                            .frame(width: 44, height: 44)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .padding(.top, -8)
                    .padding(.trailing, -8)
                    .accessibilityLabel("Close")
                    .accessibilityIdentifier("worldMapCardClose")
                }
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(Color.secondary)
                    .lineLimit(2)
                Button(action: onOpen) {
                    Label("Open Map", systemImage: "map.fill")
                        .font(.subheadline.weight(.semibold))
                        .frame(maxWidth: .infinity)
                        .frame(minHeight: 40)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .foregroundStyle(Color.black)
                .accessibilityIdentifier("worldOpenMapButton")
            }
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("worldMapCard")
        .task(id: thumbnailURL) {
            thumbnail = await MapThumbnailCache.shared.load(thumbnailURL)
        }
    }

    private var detail: String {
        var parts: [String] = []
        if let g = map.georef {
            let mid = CGPoint(x: map.pixelSize.width / 2, y: map.pixelSize.height / 2)
            let scale = MapScaleText.text(metersPerPixel: g.metersPerPixel(atPixel: mid), dpi: map.renderDPI)
            if !scale.isEmpty { parts.append(scale) }
        }
        switch MapProximity.of(map, from: here) {
        case .onMap: parts.append("You're on this map")
        case .away(let meters, let bearing):
            parts.append("\(GeoFormat.distance(meters, units: units)) \(Geodesy.compassPoint(bearing))")
        case .unknown: break
        }
        parts.append(MapStatus.text(for: map))
        return parts.joined(separator: " · ")
    }
}

/// Your maps under the world view: the located ones (tap: fly there) and, separately, the ones
/// not located yet (tap: open, to locate or calibrate). Collapsed to its header by default.
private struct WorldMapsList: View {
    let located: [MapDocument]
    let unlocated: [MapDocument]
    let here: LatLon?
    let units: DistanceUnits
    @Binding var expanded: Bool
    var onSelect: (UUID) -> Void
    var onOpen: (UUID) -> Void
    var onShowAll: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "square.stack.3d.up.fill")
                        .foregroundStyle(Theme.accent)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(located.isEmpty && unlocated.isEmpty ? "No maps yet" : "\(located.count + unlocated.count) maps")
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Color.primary)
                        Text(unlocated.isEmpty ? "Tap a map on the globe to open it"
                             : "\(unlocated.count) not located yet")
                            .font(.caption)
                            .foregroundStyle(Color.secondary)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.up")
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(Color.secondary)
                        .rotationEffect(.degrees(expanded ? 180 : 0))
                }
                .padding(.horizontal, 16)
                .frame(minHeight: 52)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("worldMapsListHeader")

            if expanded {
                Divider().overlay(Theme.stroke)
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        if !located.isEmpty {
                            sectionHeader("On the map", action: ("Show All", onShowAll))
                            ForEach(located) { map in
                                row(map, subtitle: locatedSubtitle(map), symbol: "map.fill") { onSelect(map.id) }
                                    .accessibilityIdentifier("worldLocatedRow")
                            }
                        }
                        if !unlocated.isEmpty {
                            sectionHeader("Not located yet", action: nil)
                            ForEach(unlocated) { map in
                                row(map, subtitle: "Open to locate it or place it by hand",
                                    symbol: "questionmark.circle.fill") { onOpen(map.id) }
                                    .accessibilityIdentifier("worldUnlocatedRow")
                            }
                        }
                        if located.isEmpty && unlocated.isEmpty {
                            Text("Import a map in the library: it appears here once it is located.")
                                .font(.subheadline)
                                .foregroundStyle(Color.secondary)
                                .padding(16)
                        }
                    }
                }
                .frame(maxHeight: 300)
                .accessibilityIdentifier("worldMapsList")
            }
        }
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
    }

    private func sectionHeader(_ title: String, action: (String, () -> Void)?) -> some View {
        HStack {
            Text(title.uppercased())
                .font(.caption.weight(.semibold))
                .foregroundStyle(Color.secondary)
            Spacer()
            if let action {
                Button(action.0, action: action.1)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.accent)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 4)
    }

    private func row(_ map: MapDocument, subtitle: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(map.isGeoreferenced ? Theme.accent : Theme.warn)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(map.name)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Color.primary)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(Color.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Color.secondary.opacity(0.7))
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 50)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
    }

    private func locatedSubtitle(_ map: MapDocument) -> String {
        switch MapProximity.of(map, from: here) {
        case .onMap: return "You're on this map"
        case .away(let meters, let bearing):
            return "\(GeoFormat.distance(meters, units: units)) \(Geodesy.compassPoint(bearing))"
        case .unknown: return MapStatus.text(for: map)
        }
    }
}
