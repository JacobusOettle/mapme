import CoreLocation
import SwiftUI
import UIKit

// The maps feature on the map screen (docs/design/explorer.md §7.3): "Maps here" and
// auto-switching, the walked-off banner, Settings → Maps. The canvas content (the sheet's edge,
// other maps' places, the terrain beyond the sheet) is in MapsCanvasOverlays.swift and
// OffMapTerrainLayer.swift.

/// Side column: the maps covering you (a badge with their count), switch to one, the world view.
/// Also drives auto-switching while the screen shows.
@MainActor
struct MapsHereButton: View {
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model
    /// Bumped on every switch (a selection haptic).
    @State private var switches = 0

    var body: some View {
        let listing = MapsHereListing.make(model: model, context: context, map: map)
        Menu {
            Section {
                ForEach(listing.rows) { row in
                    Button {
                        choose(row.id)
                    } label: {
                        // Two texts: the menu shows the second as the item's subtitle.
                        Label {
                            Text(row.name)
                            Text(row.detail)
                        } icon: {
                            Image(systemName: row.isCurrent ? "checkmark" : "map")
                        }
                    }
                }
                if listing.rows.isEmpty {
                    Text(listing.emptyText)
                }
            } header: {
                Text(listing.title)
            }
            Section {
                Button {
                    MapsNavigation.openWorldView(model, followingYou: false)
                } label: {
                    Label("World View", systemImage: "globe.europe.africa.fill")
                }
                .accessibilityIdentifier("mapsHereWorldView")
            }
        } label: {
            Image(systemName: "square.stack.3d.up")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Color.primary)
                .frame(width: Theme.minTapTarget, height: Theme.minTapTarget)
                .contentShape(Circle())
        }
        .floatingChrome(Circle(), interactive: true)
        .overlay(alignment: .topTrailing) {
            if listing.rows.count > 1 {
                MapsHereBadge(count: listing.rows.count)
                    .offset(x: 5, y: -5)
                    .allowsHitTesting(false)
                    .transition(.scale.combined(with: .opacity))
            }
        }
        .animation(.spring(response: 0.3, dampingFraction: 0.75), value: listing.rows.count)
        .accessibilityLabel(listing.accessibilityLabel)
        .accessibilityValue("\(listing.rows.count)")
        .accessibilityIdentifier("mapsHereButton")
        .sensoryFeedback(.selection, trigger: switches)
        .onAppear {
            model.mapSwitcher.screenDidShow(context.mapID)
        }
        .onChange(of: model.location.location) { _, fix in
            considerSwitching(fix)
        }
    }

    /// You picked a map in the menu.
    private func choose(_ id: UUID) {
        guard id != context.mapID else { return }
        model.mapSwitcher.userChose(id)
        context.switchMap(to: id)
        switches += 1
    }

    /// Every fix: switch to a more detailed map, or to the one you walked onto (the rules are in
    /// `AutoSwitchPolicy`). Only while this screen is the one on top.
    private func considerSwitching(_ fix: CLLocation?) {
        guard case .map? = model.path.last else { return }
        // Nothing presented: neither the features' sheets nor the screen's own (Nearby,
        // calibration, the georeference popover) nor the import flow.
        let idle = context.tool == nil && context.sheet == nil && context.cover == nil
            && !MapsNavigation.somethingPresented && !model.isImportFlowActive
        guard let decision = model.mapSwitcher.evaluate(currentID: context.mapID, fix: fix, tracking: context.tracking,
                                                        idle: idle, now: Date()),
              let target = model.library.map(id: decision.mapID)
        else { return }
        let previous = context.mapID
        let previousName = model.library.map(id: previous)?.name ?? map.name
        context.switchMap(to: target.id)
        model.mapSwitcher.didAutoSwitch(to: target.id)
        switches += 1
        let text: String
        switch decision.reason {
        case .moreDetailed: text = "Switched to \(target.name) · more detailed here"
        case .walkedOff: text = "Switched to \(target.name) · you walked off \(previousName)"
        }
        let switcher = model.mapSwitcher
        let screen = context
        context.show(MapToast(text: text, symbol: "square.stack.3d.up.fill", actionTitle: "Undo", action: {
            // Back, and leave that map alone until you have walked off it (or 15 min).
            switcher.undo(target.id, backTo: previous)
            screen.switchMap(to: previous)
        }))
    }
}

/// The count on the "Maps here" button.
private struct MapsHereBadge: View {
    let count: Int

    var body: some View {
        Text("\(count)")
            .font(.caption2.weight(.bold))
            .monospacedDigit()
            .foregroundStyle(Color.black)
            .padding(.horizontal, 5)
            .frame(minWidth: 18, minHeight: 18)
            .background(Theme.accent, in: Capsule())
            .overlay(Capsule().stroke(Color.black.opacity(0.25), lineWidth: 0.5))
            .accessibilityHidden(true)
    }
}

/// What "Maps here" lists: the maps covering you — or the middle of the view when you are not
/// near this map — most detailed first.
struct MapsHereListing {
    struct Row: Identifiable {
        var id: UUID
        var name: String
        /// "1:50 000 · You're here", "4 m/px · 2.3 km away".
        var detail: String
        var isCurrent: Bool
    }

    var title: String
    var rows: [Row]
    var emptyText: String

    var accessibilityLabel: String {
        switch rows.count {
        case 0: return "Maps here: none. World view"
        case 1: return "Maps here: 1 map"
        default: return "Maps here: \(rows.count) maps"
        }
    }

    @MainActor
    static func make(model: AppModel, context: MapScreenContext, map: MapDocument) -> MapsHereListing {
        let units = model.settings.units
        let located = model.library.maps.filter(\.isGeoreferenced)
        let fix = model.location.location
        let here = fix.map { LatLon($0.coordinate) }

        // Near: some map holds you, or you are on this map's sheet or the terrain continuing it.
        var position: LatLon?
        var atYou = false
        if let here {
            let margin = MapContinuation.marginPixels(for: map, enabled: model.settings.showOffMapTerrain)
            let onThis = map.georef.map { g -> Bool in
                let px = g.pixel(for: here)
                let area = CGRect(origin: .zero, size: map.pixelSize).insetBy(dx: -margin, dy: -margin)
                return px.x.isFinite && px.y.isFinite && area.contains(px)
            } ?? false
            if onThis || !model.mapSwitcher.mapsHere.isEmpty {
                position = here
                atYou = true
            }
        }
        if position == nil, let g = map.georef, let camera = context.controller.canvas?.camera {
            let centre = g.latLon(forPixel: camera.center)
            if centre.lat.isFinite, centre.lon.isFinite { position = centre }
        }
        guard let spot = position else {
            return MapsHereListing(title: "Maps here", rows: [],
                                   emptyText: map.georef == nil ? "This map isn't located yet" : "Waiting for your position")
        }
        let coverages = MapSwitcher.covering(spot, maps: located)
        let rows = coverages.compactMap { c -> Row? in
            guard let doc = located.first(where: { $0.id == c.mapID }) else { return nil }
            var parts = [MapScaleText.text(metersPerPixel: c.metersPerPixel, dpi: doc.renderDPI)]
            if atYou {
                parts.append("You're here")
            } else if let here, let cover = MapSwitcher.coverage(of: doc, at: here) {
                parts.append(cover.insideMarginM >= 0 ? "You're here"
                    : GeoFormat.distance(-cover.insideMarginM, units: units) + " away")
            }
            return Row(id: doc.id, name: doc.name, detail: parts.filter { !$0.isEmpty }.joined(separator: " · "),
                       isCurrent: doc.id == context.mapID)
        }
        return MapsHereListing(title: atYou ? "Maps here" : "Maps at the middle of the view", rows: rows,
                               emptyText: atYou ? "No located map covers you" : "No located map covers this spot")
    }
}

/// Under the title when you are off the sheet: "You've walked off Grootkloof · showing terrain",
/// with the maps covering you or the world view.
@MainActor
struct WalkedOffMapBanner: View {
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model

    var body: some View {
        let margin = MapContinuation.marginPixels(for: map, enabled: model.settings.showOffMapTerrain)
        if let whereabouts = MapWhereabouts.of(map, fix: model.location.location, continuation: margin),
           !whereabouts.isOnSheet {
            let others = model.mapSwitcher.mapsHere.filter { $0.mapID != context.mapID }
            let target = others.first.flatMap { model.library.map(id: $0.mapID) }
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "figure.walk.departure")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.warn)
                        .frame(width: 24)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title(whereabouts))
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Color.primary)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(subtitle(whereabouts))
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Color.secondary)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                HStack(spacing: 8) {
                    if let target {
                        Button {
                            model.mapSwitcher.userChose(target.id)
                            context.switchMap(to: target.id)
                        } label: {
                            Label("Switch to \(target.name)", systemImage: "square.stack.3d.up.fill")
                                .lineLimit(1)
                        }
                        .buttonStyle(BannerChipStyle(prominent: true))
                        .accessibilityIdentifier("walkedOffSwitchButton")
                    }
                    Button {
                        MapsNavigation.openWorldView(model, followingYou: true)
                    } label: {
                        Label("World View", systemImage: "globe.europe.africa.fill")
                            .lineLimit(1)
                    }
                    .buttonStyle(BannerChipStyle(prominent: target == nil))
                    .accessibilityIdentifier("walkedOffWorldButton")
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .floatingChrome(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("walkedOffBanner")
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    private func title(_ w: MapWhereabouts) -> String {
        switch w {
        case .onSheet, .offSheet: return "You've walked off \(map.name)"
        case .beyond(let meters): return "You're \(GeoFormat.distance(meters, units: model.settings.units)) from \(map.name)"
        }
    }

    private func subtitle(_ w: MapWhereabouts) -> String {
        let units = model.settings.units
        switch w {
        case .onSheet:
            return ""
        case .offSheet(let meters):
            let distance = GeoFormat.distance(meters, units: units) + " beyond the edge"
            switch terrainState {
            case .shown: return "Showing terrain · " + distance
            case .loading: return "Loading terrain · " + distance
            case .unavailable, .off: return "Terrain here isn't on your iPhone · " + distance
            }
        case .beyond:
            if !model.settings.showOffMapTerrain { return "Terrain beyond maps is off in Settings" }
            return "Beyond this map's terrain · the world view shows where you are"
        }
    }

    /// What the terrain layer of this screen shows.
    private var terrainState: OffMapTerrainStatus.State {
        let layer = context.overlays?.mapSpaceLayers.lazy.compactMap { $0 as? OffMapTerrainLayer }.first
        return layer?.status.state ?? .off
    }
}

/// A small capsule button inside a banner.
private struct BannerChipStyle: ButtonStyle {
    var prominent: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.footnote.weight(.semibold))
            .foregroundStyle(prominent ? Color.black : Theme.accent)
            .padding(.horizontal, 12)
            .frame(minHeight: 32)
            .background(prominent ? Theme.accent : Theme.accent.opacity(0.14), in: Capsule())
            .contentShape(Capsule())
            .scaleEffect(configuration.isPressed ? 0.95 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

/// Opening the world view from a map screen; what is on screen.
@MainActor
enum MapsNavigation {
    /// A sheet, cover, popover or alert is up in the active window (the map is not what you look at).
    static var somethingPresented: Bool {
        for scene in UIApplication.shared.connectedScenes {
            guard let windows = (scene as? UIWindowScene)?.windows, scene.activationState == .foregroundActive else {
                continue
            }
            if windows.contains(where: { $0.isKeyWindow && $0.rootViewController?.presentedViewController != nil }) {
                return true
            }
        }
        return false
    }

    /// Pushes the world view; `followingYou` centres it on you and keeps following (from the
    /// walked-off banner: "the terrain view when you are far from every map").
    static func openWorldView(_ model: AppModel, followingYou: Bool) {
        model.mapSwitcher.worldViewFollowsUser = followingYou
        if model.path.last != .world {
            model.path.append(.world)
        }
    }
}

/// Settings → Maps: switch maps automatically, terrain beyond the sheet, other maps' places.
@MainActor
struct MapsSettingsSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var settings = model.settings
        Section {
            Toggle(isOn: $settings.autoSwitchMaps) {
                SettingsLabel("Switch maps automatically", symbol: "square.stack.3d.up.fill", color: .indigo)
            }
            .accessibilityIdentifier("autoSwitchMapsToggle")
            Toggle(isOn: $settings.showOffMapTerrain) {
                SettingsLabel("Terrain beyond the map's edge", symbol: "mountain.2.fill", color: .brown)
            }
            .accessibilityIdentifier("offMapTerrainToggle")
            Toggle(isOn: $settings.showOtherMapsPOIs) {
                SettingsLabel("Places from your other maps", symbol: "mappin.and.ellipse", color: .teal)
            }
            .accessibilityIdentifier("otherMapsPOIsToggle")
        } header: {
            Text("Maps")
        } footer: {
            Text("While MapMe follows you, it switches to the most detailed map you're on, or to the one you walk onto, and says so with Undo. Beyond a map's edge it keeps drawing contours from the terrain, with your track, routes and the places on your other maps.")
        }
    }
}
