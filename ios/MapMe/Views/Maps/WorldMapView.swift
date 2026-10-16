import CoreLocation
import MapKit
import SwiftUI
import UIKit

// The world view's map (docs/design/explorer.md §7.3, World view): an MKMapView — SwiftUI's `Map`
// cannot take a tile overlay — in Apple's dark, muted standard style, with
//   • a terrain overlay (contours + hillshade drawn by the core from the DEM, so it works offline
//     from the terrain on the device where Apple's base map is blank),
//   • each located map's footprint with its thumbnail drawn in and an accent outline,
//   • routes, the recorded track, waypoints and notes (clustered), offline regions (dashed), you,
//   • the places read off your maps and fetched for them (POIs, clustered), so a hiker who walked
//     beyond every sheet still sees the huts, peaks and water around.

/// What the world view's map shows; the coordinator rebuilds a group of overlays only when its
/// key changes.
struct WorldMapState: Equatable {
    /// Located maps (ids, names, georeferences) and the thumbnail appearance.
    var mapsKey: Int
    var dark: Bool
    /// `GeoItemStore.revision`.
    var itemsKey: Int
    /// The recorded track's length and last point (redrawn on its own while you record).
    var trackKey: Int
    var regionsKey: Int
    /// The located maps' POIs (their count per map).
    var poisKey: Int
    /// Settings → POIs on maps.
    var showsPOIs: Bool
    var showsTerrain: Bool
    var showsRegions: Bool
    var showsItems: Bool
    var terrainZoomCap: Int
    var selectedMapID: UUID?
}

/// A one-shot camera move (a new `nonce` moves again).
struct WorldFocus: Equatable {
    enum Target: Equatable {
        case map(UUID)
        case user
        /// Every footprint (and you).
        case everything
    }

    var target: Target
    var nonce = UUID()
}

struct WorldMapView: UIViewRepresentable {
    let model: AppModel
    var state: WorldMapState
    var user: CLLocation?
    var focus: WorldFocus?
    @Binding var followsUser: Bool
    var onSelectMap: (UUID?) -> Void

    func makeCoordinator() -> WorldMapCoordinator {
        WorldMapCoordinator(model: model)
    }

    func makeUIView(context: Context) -> MKMapView {
        let view = MKMapView(frame: .zero)
        context.coordinator.install(on: view)
        return view
    }

    func updateUIView(_ view: MKMapView, context: Context) {
        let coordinator = context.coordinator
        coordinator.onSelectMap = onSelectMap
        let binding = $followsUser
        coordinator.onFollowChange = { value in
            // Never write SwiftUI state during a view update.
            DispatchQueue.main.async { binding.wrappedValue = value }
        }
        coordinator.update(view, state: state, user: user, focus: focus, followsUser: followsUser)
    }

    static func dismantleUIView(_ view: MKMapView, coordinator: WorldMapCoordinator) {
        coordinator.onSelectMap = nil
        coordinator.onFollowChange = nil
        view.delegate = nil
    }
}

/// Keeps the MKMapView's overlays and annotations in step with the app's data.
@MainActor
final class WorldMapCoordinator: NSObject, MKMapViewDelegate, UIGestureRecognizerDelegate {
    var onSelectMap: ((UUID?) -> Void)?
    var onFollowChange: ((Bool) -> Void)?

    private weak var model: AppModel?
    private var state: WorldMapState?
    private var terrain: TerrainContourTileOverlay?
    private var footprints: [UUID: MapFootprintOverlay] = [:]
    private var footprintRenderers: [UUID: MapFootprintRenderer] = [:]
    private var mapLabels: [WorldPinAnnotation] = []
    private var routeLines: [WorldRouteLine] = []
    private var trackLine: WorldRouteLine?
    private var itemPins: [WorldPinAnnotation] = []
    private var poiPins: [WorldPinAnnotation] = []
    private var regionShapes: [WorldRegionPolygon] = []
    private let userPin = WorldPinAnnotation(kind: .user, id: "you", title: "You",
                                             coordinate: CLLocationCoordinate2D(latitude: 0, longitude: 0))
    private var userShown = false
    /// The latest fix shown.
    private var lastUser: CLLocation?
    private var followsUser = false
    private var lastFocus: UUID?
    private var didFrameInitially = false
    private var thumbnailTasks: [UUID: Task<Void, Never>] = [:]
    /// Terrain arriving later (a region finished, the network is back): the overlay loads again.
    private var arrival: TerrainArrivalObserver?

    init(model: AppModel) {
        self.model = model
    }

    func install(on view: MKMapView) {
        view.delegate = self
        view.overrideUserInterfaceStyle = .dark
        let config = MKStandardMapConfiguration(elevationStyle: .realistic, emphasisStyle: .muted)
        config.pointOfInterestFilter = .excludingAll
        view.preferredConfiguration = config
        view.showsCompass = true
        view.showsScale = true
        view.isPitchEnabled = true
        view.accessibilityIdentifier = "worldMap"
        view.register(MKMarkerAnnotationView.self, forAnnotationViewWithReuseIdentifier: "item")
        view.register(MKMarkerAnnotationView.self,
                      forAnnotationViewWithReuseIdentifier: MKMapViewDefaultClusterAnnotationViewReuseIdentifier)
        let tap = UITapGestureRecognizer(target: self, action: #selector(handleTap(_:)))
        tap.delegate = self
        view.addGestureRecognizer(tap)
        if let model {
            arrival = TerrainArrivalObserver(model.terrain) { [weak self, weak view] in
                guard let self, let view, let terrain = self.terrain,
                      let renderer = view.renderer(for: terrain) as? MKTileOverlayRenderer
                else { return }
                renderer.reloadData()
            }
        }
    }

    // MARK: - Updates

    func update(_ view: MKMapView, state new: WorldMapState, user: CLLocation?, focus: WorldFocus?, followsUser: Bool) {
        guard let model else { return }
        let old = state
        state = new

        if old?.showsTerrain != new.showsTerrain || old?.dark != new.dark || old?.terrainZoomCap != new.terrainZoomCap {
            if let terrain { view.removeOverlay(terrain) }
            terrain = nil
            if new.showsTerrain {
                let overlay = TerrainContourTileOverlay(provider: model.terrain, dark: new.dark, zoomCap: new.terrainZoomCap)
                view.insertOverlay(overlay, at: 0, level: .aboveRoads)
                terrain = overlay
            }
        }
        if old?.regionsKey != new.regionsKey || old?.showsRegions != new.showsRegions {
            rebuildRegions(view, shown: new.showsRegions)
        }
        if old?.mapsKey != new.mapsKey || old?.dark != new.dark {
            rebuildFootprints(view, dark: new.dark)
        }
        if old?.itemsKey != new.itemsKey || old?.showsItems != new.showsItems {
            rebuildItems(view, shown: new.showsItems)
        }
        if old?.poisKey != new.poisKey || old?.showsItems != new.showsItems || old?.showsPOIs != new.showsPOIs {
            rebuildPOIs(view, shown: new.showsItems && new.showsPOIs)
        }
        if old?.trackKey != new.trackKey || old?.showsItems != new.showsItems {
            rebuildTrack(view, shown: new.showsItems)
        }
        if old?.selectedMapID != new.selectedMapID {
            for (id, renderer) in footprintRenderers {
                renderer.setSelected(id == new.selectedMapID)
            }
        }

        updateUser(view, user)
        if followsUser != self.followsUser {
            self.followsUser = followsUser
            if followsUser, let user { center(view, on: user.coordinate, animated: true) }
        } else if followsUser, let user {
            view.setCenter(user.coordinate, animated: true)
        }

        if !didFrameInitially {
            didFrameInitially = true
            // The first update comes before layout: frame once the view has its size (following
            // you, if that was asked meanwhile).
            Task { @MainActor [weak self, weak view] in
                for _ in 0..<40 {
                    guard let view, view.bounds.width <= 0 else { break }
                    try? await Task.sleep(nanoseconds: 50_000_000)
                }
                guard let self, let view else { return }
                if self.followsUser, let here = self.lastUser {
                    view.setRegion(MKCoordinateRegion(center: here.coordinate, latitudinalMeters: 12_000,
                                                      longitudinalMeters: 12_000), animated: false)
                } else {
                    self.frameEverything(view, user: self.lastUser, animated: false)
                }
            }
        }
        if let focus, focus.nonce != lastFocus {
            lastFocus = focus.nonce
            switch focus.target {
            case .map(let id):
                if let footprint = footprints[id] {
                    view.setVisibleMapRect(footprint.boundingMapRect, edgePadding: Self.padding, animated: true)
                }
            case .user:
                if let user { center(view, on: user.coordinate, animated: true) }
            case .everything:
                frameEverything(view, user: user, animated: true)
            }
        }
    }

    /// Room for the floating controls around a framed area.
    private static let padding = UIEdgeInsets(top: 120, left: 40, bottom: 240, right: 70)

    private func center(_ view: MKMapView, on coordinate: CLLocationCoordinate2D, animated: Bool) {
        // Zoomed far out: come in to walking terrain; otherwise keep the zoom.
        if view.region.span.latitudeDelta > 0.5 {
            view.setRegion(MKCoordinateRegion(center: coordinate, latitudinalMeters: 12_000, longitudinalMeters: 12_000),
                           animated: animated)
        } else {
            view.setCenter(coordinate, animated: animated)
        }
    }

    private func frameEverything(_ view: MKMapView, user: CLLocation?, animated: Bool) {
        var rect = MKMapRect.null
        for footprint in footprints.values {
            rect = rect.union(footprint.boundingMapRect)
        }
        if let user {
            let p = MKMapPoint(user.coordinate)
            rect = rect.union(MKMapRect(x: p.x, y: p.y, width: 0, height: 0))
        }
        guard !rect.isNull else { return }
        // At least ~20 km across, so a lone small sheet is shown with its surroundings.
        let minimum = 20_000 * MKMapPointsPerMeterAtLatitude(rect.origin.coordinate.latitude)
        if rect.size.width < minimum || rect.size.height < minimum {
            let mid = MKMapPoint(x: rect.midX, y: rect.midY)
            let side = max(minimum, max(rect.size.width, rect.size.height))
            rect = MKMapRect(x: mid.x - side / 2, y: mid.y - side / 2, width: side, height: side)
        }
        view.setVisibleMapRect(rect, edgePadding: Self.padding, animated: animated)
    }

    private func rebuildFootprints(_ view: MKMapView, dark: Bool) {
        guard let model else { return }
        view.removeOverlays(Array(footprints.values))
        view.removeAnnotations(mapLabels)
        for task in thumbnailTasks.values { task.cancel() }
        thumbnailTasks = [:]
        footprints = [:]
        footprintRenderers = [:]
        mapLabels = []
        // Coarser maps first, so a detailed sheet lies on top of the overview it sits in.
        let located = model.library.maps.filter(\.isGeoreferenced)
        let ordered = located.map { map -> (MapDocument, Double) in
            let mpp = map.georef?.metersPerPixel(atPixel: CGPoint(x: map.pixelSize.width / 2, y: map.pixelSize.height / 2)) ?? 0
            return (map, mpp.isFinite ? mpp : 0)
        }.sorted { $0.1 > $1.1 }
        for (map, _) in ordered {
            guard let footprint = MapFootprintOverlay(map: map) else { continue }
            footprints[map.id] = footprint
            view.addOverlay(footprint, level: .aboveRoads)
            let label = WorldPinAnnotation(kind: .map, id: map.id.uuidString, title: map.name,
                                           coordinate: footprint.labelCoordinate)
            mapLabels.append(label)
            let url = dark ? model.library.darkThumbnailURL(for: map) : model.library.thumbnailURL(for: map)
            let lightURL = model.library.thumbnailURL(for: map)
            let id = map.id
            thumbnailTasks[id] = Task { @MainActor [weak self] in
                var image = await MapThumbnailCache.shared.load(url)
                if image == nil, url != lightURL { image = await MapThumbnailCache.shared.load(lightURL) }
                guard !Task.isCancelled, let self, let cg = image?.cgImage else { return }
                self.footprints[id]?.setThumbnail(cg)
                self.footprintRenderers[id]?.setNeedsDisplay()
            }
        }
        view.addAnnotations(mapLabels)
    }

    private func rebuildItems(_ view: MKMapView, shown: Bool) {
        guard let model else { return }
        view.removeOverlays(routeLines)
        view.removeAnnotations(itemPins)
        routeLines = []
        itemPins = []
        guard shown else { return }
        let store = model.geoItems
        for route in store.routes {
            if let line = WorldRouteLine.make(route.latLons.map(\.coordinate), color: route.displayColor.uiColor, width: 4) {
                routeLines.append(line)
            }
        }
        view.addOverlays(routeLines, level: .aboveLabels)
        for w in store.waypoints {
            itemPins.append(WorldPinAnnotation(kind: .waypoint(w.symbol.systemImage), id: w.id.uuidString,
                                               title: w.name, coordinate: w.position.coordinate))
        }
        for n in store.notes {
            itemPins.append(WorldPinAnnotation(kind: .note(hasPhoto: n.photo != nil), id: n.id.uuidString,
                                               title: n.title, coordinate: n.position.coordinate))
        }
        view.addAnnotations(itemPins)
    }

    /// The places of your located maps (names read off them, OpenStreetMap), each place once: the
    /// same name within 150 m on two sheets is one pin (`OtherMapsPOIOverlay.DuplicateIndex`).
    private func rebuildPOIs(_ view: MKMapView, shown: Bool) {
        guard let model else { return }
        view.removeAnnotations(poiPins)
        poiPins = []
        guard shown else { return }
        var seen = OtherMapsPOIOverlay.DuplicateIndex()
        for map in model.library.maps where map.isGeoreferenced {
            for poi in map.pois where poi.source != .user && poi.kind != .waypoint {
                guard poi.position.lat.isFinite, poi.position.lon.isFinite, !seen.contains(poi) else { continue }
                seen.insert(poi)
                poiPins.append(WorldPinAnnotation(kind: .poi(poi.kind.symbolName), id: poi.id.uuidString,
                                                  title: poi.name.isEmpty ? nil : poi.name,
                                                  coordinate: poi.position.coordinate))
            }
        }
        view.addAnnotations(poiPins)
    }

    /// Your recorded track (pink, as on the map).
    private func rebuildTrack(_ view: MKMapView, shown: Bool) {
        guard let model else { return }
        if let trackLine { view.removeOverlay(trackLine) }
        trackLine = nil
        guard shown, let line = WorldRouteLine.make(model.tracks.current.points.map { $0.latLon.coordinate },
                                                    color: Theme.uiTrack, width: 4)
        else { return }
        trackLine = line
        view.addOverlay(line, level: .aboveLabels)
    }

    private func rebuildRegions(_ view: MKMapView, shown: Bool) {
        guard let model else { return }
        view.removeOverlays(regionShapes)
        regionShapes = []
        guard shown else { return }
        for region in model.offline.regions {
            regionShapes.append(contentsOf: WorldRegionPolygon.make(region.bounds))
        }
        view.addOverlays(regionShapes, level: .aboveRoads)
    }

    private func updateUser(_ view: MKMapView, _ user: CLLocation?) {
        lastUser = user
        guard let user else {
            if userShown {
                view.removeAnnotation(userPin)
                userShown = false
            }
            return
        }
        userPin.coordinate = user.coordinate
        if !userShown {
            view.addAnnotation(userPin)
            userShown = true
        }
    }

    // MARK: - Taps

    @objc private func handleTap(_ g: UITapGestureRecognizer) {
        guard let view = g.view as? MKMapView, g.state == .ended else { return }
        let point = g.location(in: view)
        // Annotations handle their own taps.
        if let hit = view.hitTest(point, with: nil), hit is MKAnnotationView || hit.superview is MKAnnotationView { return }
        let mapPoint = MKMapPoint(view.convert(point, toCoordinateFrom: view))
        let hits = footprints.values.filter { $0.contains(mapPoint) }
        // The most detailed sheet under the finger (the smallest footprint).
        let pick = hits.min { $0.boundingMapRect.size.width * $0.boundingMapRect.size.height
            < $1.boundingMapRect.size.width * $1.boundingMapRect.size.height }
        onSelectMap?(pick?.mapID)
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer) -> Bool {
        true
    }

    // MARK: - MKMapViewDelegate

    func mapView(_ mapView: MKMapView, rendererFor overlay: MKOverlay) -> MKOverlayRenderer {
        if let tiles = overlay as? TerrainContourTileOverlay {
            return MKTileOverlayRenderer(tileOverlay: tiles)
        }
        if let footprint = overlay as? MapFootprintOverlay {
            let renderer = MapFootprintRenderer(overlay: footprint)
            renderer.setSelected(footprint.mapID == state?.selectedMapID)
            footprintRenderers[footprint.mapID] = renderer
            return renderer
        }
        if let line = overlay as? WorldRouteLine {
            let renderer = MKPolylineRenderer(polyline: line)
            renderer.strokeColor = line.color
            renderer.lineWidth = line.width
            renderer.lineCap = .round
            renderer.lineJoin = .round
            return renderer
        }
        if let region = overlay as? WorldRegionPolygon {
            let renderer = MKPolygonRenderer(polygon: region)
            renderer.strokeColor = Theme.uiAccent.withAlphaComponent(0.9)
            renderer.fillColor = Theme.uiAccent.withAlphaComponent(0.06)
            renderer.lineWidth = 1.5
            renderer.lineDashPattern = [6, 5]
            return renderer
        }
        return MKOverlayRenderer(overlay: overlay)
    }

    func mapView(_ mapView: MKMapView, viewFor annotation: MKAnnotation) -> MKAnnotationView? {
        if let cluster = annotation as? MKClusterAnnotation {
            let view = mapView.dequeueReusableAnnotationView(
                withIdentifier: MKMapViewDefaultClusterAnnotationViewReuseIdentifier, for: cluster)
            // Clusters form per identifier: all places (yellow, as on the map) or all your items.
            let places = cluster.memberAnnotations.contains { member in
                guard let pin = member as? WorldPinAnnotation, case .poi = pin.kind else { return false }
                return true
            }
            if let marker = view as? MKMarkerAnnotationView {
                marker.markerTintColor = places ? Theme.uiPOI.withAlphaComponent(0.9) : Theme.uiNote
                marker.glyphTintColor = places ? .black : .white
            }
            return view
        }
        guard let pin = annotation as? WorldPinAnnotation else { return nil }
        switch pin.kind {
        case .user:
            let view = mapView.dequeueReusableAnnotationView(withIdentifier: "user") as? WorldUserAnnotationView
                ?? WorldUserAnnotationView(annotation: pin, reuseIdentifier: "user")
            view.annotation = pin
            view.zPriority = .max
            return view
        case .map:
            let view = mapView.dequeueReusableAnnotationView(withIdentifier: "mapLabel") as? WorldMapLabelView
                ?? WorldMapLabelView(annotation: pin, reuseIdentifier: "mapLabel")
            view.annotation = pin
            view.setText(pin.title ?? "")
            return view
        case .waypoint(let symbol):
            let view = mapView.dequeueReusableAnnotationView(withIdentifier: "item", for: pin)
            if let marker = view as? MKMarkerAnnotationView {
                marker.markerTintColor = Theme.uiUserRed
                marker.glyphTintColor = .white
                marker.glyphImage = UIImage(systemName: symbol)
                marker.clusteringIdentifier = "geoItems"
                marker.displayPriority = .defaultHigh
                marker.titleVisibility = .adaptive
            }
            return view
        case .note(let hasPhoto):
            let view = mapView.dequeueReusableAnnotationView(withIdentifier: "item", for: pin)
            if let marker = view as? MKMarkerAnnotationView {
                marker.markerTintColor = Theme.uiNote
                marker.glyphTintColor = .white
                marker.glyphImage = UIImage(systemName: hasPhoto ? "photo.fill" : "note.text")
                marker.clusteringIdentifier = "geoItems"
                marker.displayPriority = .defaultLow
                marker.titleVisibility = .adaptive
            }
            return view
        case .poi(let symbol):
            let view = mapView.dequeueReusableAnnotationView(withIdentifier: "item", for: pin)
            if let marker = view as? MKMarkerAnnotationView {
                marker.markerTintColor = Theme.uiPOI.withAlphaComponent(0.9)
                marker.glyphTintColor = .black
                marker.glyphImage = UIImage(systemName: symbol)
                // Their own clusters: a valley's dozen places never hide your waypoints.
                marker.clusteringIdentifier = "mapPOIs"
                marker.displayPriority = .defaultLow
                marker.titleVisibility = .adaptive
            }
            return view
        }
    }

    func mapView(_ mapView: MKMapView, didSelect view: MKAnnotationView) {
        guard let pin = view.annotation as? WorldPinAnnotation, case .map = pin.kind else { return }
        mapView.deselectAnnotation(pin, animated: false)
        onSelectMap?(UUID(uuidString: pin.id))
    }

    func mapView(_ mapView: MKMapView, regionWillChangeAnimated animated: Bool) {
        // A pan or pinch by hand stops following, as in Apple Maps.
        guard followsUser else { return }
        let touching = mapView.subviews.first?.gestureRecognizers?.contains {
            $0.state == .began || $0.state == .changed
        } ?? false
        if touching {
            followsUser = false
            onFollowChange?(false)
        }
    }
}

// MARK: - Footprints

/// A located map's footprint: its four corners (TL, TR, BR, BL) through the georeference.
final class MapFootprintOverlay: NSObject, MKOverlay {
    let mapID: UUID
    let name: String
    /// TL, TR, BR, BL, unwrapped across the antimeridian (x may exceed the world's width).
    let corners: [MKMapPoint]
    let coordinate: CLLocationCoordinate2D
    let boundingMapRect: MKMapRect
    /// Where the map's name sits: the middle of its top edge, a little inside.
    let labelCoordinate: CLLocationCoordinate2D
    private let lock = NSLock()
    private var image: CGImage?

    init?(map: MapDocument) {
        guard let g = map.georef, map.pixelWidth > 0, map.pixelHeight > 0 else { return nil }
        let w = CGFloat(map.pixelWidth)
        let h = CGFloat(map.pixelHeight)
        let latLons = [CGPoint.zero, CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)].map {
            g.latLon(forPixel: $0)
        }
        guard latLons.allSatisfy({ $0.lat.isFinite && $0.lon.isFinite && abs($0.lat) < 85 }) else { return nil }
        var points = latLons.map { MKMapPoint($0.coordinate) }
        // A sheet across 180°: put every corner on the side of the first.
        let world = MKMapSize.world.width
        for i in 1..<points.count where abs(points[i].x - points[0].x) > world / 2 {
            points[i].x += points[i].x < points[0].x ? world : -world
        }
        mapID = map.id
        name = map.name
        corners = points
        let xs = points.map(\.x)
        let ys = points.map(\.y)
        let rect = MKMapRect(x: xs.min() ?? 0, y: ys.min() ?? 0, width: (xs.max() ?? 0) - (xs.min() ?? 0),
                             height: (ys.max() ?? 0) - (ys.min() ?? 0))
        boundingMapRect = rect
        coordinate = MKMapPoint(x: rect.midX, y: rect.midY).coordinate
        let top = MKMapPoint(x: (points[0].x + points[1].x) / 2 * 0.85 + (points[2].x + points[3].x) / 2 * 0.15,
                             y: (points[0].y + points[1].y) / 2 * 0.85 + (points[2].y + points[3].y) / 2 * 0.15)
        labelCoordinate = top.coordinate
        super.init()
    }

    /// The map's thumbnail, drawn into the footprint (set on the main thread, read by the
    /// renderer's drawing threads).
    func setThumbnail(_ cg: CGImage) {
        lock.lock()
        image = cg
        lock.unlock()
    }

    var thumbnail: CGImage? {
        lock.lock()
        defer { lock.unlock() }
        return image
    }

    /// Inside the quadrilateral (convex for any sane georeference).
    func contains(_ p: MKMapPoint) -> Bool {
        var sign = 0.0
        for i in 0..<corners.count {
            let a = corners[i]
            let b = corners[(i + 1) % corners.count]
            let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
            if cross == 0 { continue }
            if sign == 0 {
                sign = cross
            } else if (cross > 0) != (sign > 0) {
                return false
            }
        }
        return true
    }
}

/// Draws a footprint: the thumbnail mapped onto the quadrilateral (an affine from three corners
/// is exact enough at this scale), clipped to it, with an accent outline.
final class MapFootprintRenderer: MKOverlayRenderer {
    private let lock = NSLock()
    private var selected = false

    func setSelected(_ value: Bool) {
        lock.lock()
        let changed = selected != value
        selected = value
        lock.unlock()
        if changed { setNeedsDisplay() }
    }

    override func draw(_ mapRect: MKMapRect, zoomScale: MKZoomScale, in context: CGContext) {
        guard let footprint = overlay as? MapFootprintOverlay, footprint.corners.count == 4 else { return }
        let p = footprint.corners.map { point(for: $0) }
        let path = CGMutablePath()
        path.addLines(between: p)
        path.closeSubpath()
        lock.lock()
        let isSelected = selected
        lock.unlock()

        context.saveGState()
        context.addPath(path)
        context.clip()
        if let image = footprint.thumbnail {
            context.setAlpha(0.92)
            // Image unit square (y down) → the footprint: TL + u·(TR − TL) + v·(BL − TL).
            context.concatenate(CGAffineTransform(a: p[1].x - p[0].x, b: p[1].y - p[0].y,
                                                  c: p[3].x - p[0].x, d: p[3].y - p[0].y,
                                                  tx: p[0].x, ty: p[0].y))
            // CGContext.draw puts an image's first row at the top of a y-up rectangle.
            context.translateBy(x: 0, y: 1)
            context.scaleBy(x: 1, y: -1)
            context.interpolationQuality = .medium
            context.draw(image, in: CGRect(x: 0, y: 0, width: 1, height: 1))
        } else {
            context.setFillColor(Theme.uiNightPaper.withAlphaComponent(0.85).cgColor)
            context.fill(path.boundingBoxOfPath)
        }
        context.restoreGState()

        context.addPath(path)
        context.setStrokeColor((isSelected ? UIColor.white : Theme.uiAccent).cgColor)
        context.setLineWidth((isSelected ? 3.5 : 2) / zoomScale)
        context.setLineJoin(.round)
        context.strokePath()
    }
}

// MARK: - Lines, regions, pins

/// A route or track on the world view.
final class WorldRouteLine: MKPolyline {
    private(set) var color: UIColor = Theme.uiRoute
    private(set) var width: CGFloat = 4

    static func make(_ coordinates: [CLLocationCoordinate2D], color: UIColor, width: CGFloat) -> WorldRouteLine? {
        guard coordinates.count > 1 else { return nil }
        let made = coordinates.withUnsafeBufferPointer { buffer -> WorldRouteLine? in
            guard let base = buffer.baseAddress else { return nil }
            return WorldRouteLine(coordinates: base, count: buffer.count)
        }
        guard let line = made else { return nil }
        line.color = color
        line.width = width
        return line
    }
}

/// An offline region's rectangle (two when it crosses the antimeridian).
final class WorldRegionPolygon: MKPolygon {
    static func make(_ b: GeoBounds) -> [WorldRegionPolygon] {
        let parts: [(Double, Double)] = b.crossesAntimeridian ? [(b.west, 180), (-180, b.east)] : [(b.west, b.east)]
        return parts.map { west, east in
            let ring = [CLLocationCoordinate2D(latitude: b.north, longitude: west),
                        CLLocationCoordinate2D(latitude: b.north, longitude: east),
                        CLLocationCoordinate2D(latitude: b.south, longitude: east),
                        CLLocationCoordinate2D(latitude: b.south, longitude: west)]
            return ring.withUnsafeBufferPointer { buffer in
                WorldRegionPolygon(coordinates: buffer.baseAddress!, count: buffer.count)
            }
        }
    }
}

/// A pin on the world view: you, a map's name, a waypoint, a note or a place on one of your maps.
final class WorldPinAnnotation: NSObject, MKAnnotation {
    enum Kind: Equatable {
        case user
        case map
        /// The waypoint's SF Symbol.
        case waypoint(String)
        case note(hasPhoto: Bool)
        /// A place read off one of your maps or fetched for it (its kind's SF Symbol).
        case poi(String)
    }

    let kind: Kind
    let id: String
    let title: String?
    @objc dynamic var coordinate: CLLocationCoordinate2D

    init(kind: Kind, id: String, title: String?, coordinate: CLLocationCoordinate2D) {
        self.kind = kind
        self.id = id
        self.title = title
        self.coordinate = coordinate
        super.init()
    }
}

/// You: the app's red puck (white ring, soft halo), from MapMe's own location (mocks included).
final class WorldUserAnnotationView: MKAnnotationView {
    override init(annotation: MKAnnotation?, reuseIdentifier: String?) {
        super.init(annotation: annotation, reuseIdentifier: reuseIdentifier)
        let side: CGFloat = 22
        frame = CGRect(x: 0, y: 0, width: side, height: side)
        let format = UIGraphicsImageRendererFormat()
        format.opaque = false
        image = UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { ctx in
            let cg = ctx.cgContext
            cg.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.45).cgColor)
            UIColor.white.setFill()
            cg.fillEllipse(in: CGRect(x: 2, y: 2, width: side - 4, height: side - 4))
            cg.setShadow(offset: .zero, blur: 0, color: nil)
            Theme.uiUserRed.setFill()
            cg.fillEllipse(in: CGRect(x: 5, y: 5, width: side - 10, height: side - 10))
        }
        canShowCallout = false
        isAccessibilityElement = true
        accessibilityLabel = "Your position"
        accessibilityIdentifier = "worldUserLocation"
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }
}

/// A map's name over its footprint: a small glass capsule with a map glyph; tap selects the map.
final class WorldMapLabelView: MKAnnotationView {
    private let label = UILabel()
    private let icon = UIImageView(image: UIImage(systemName: "map.fill"))
    private let blur = UIVisualEffectView(effect: UIBlurEffect(style: .systemThinMaterialDark))

    override init(annotation: MKAnnotation?, reuseIdentifier: String?) {
        super.init(annotation: annotation, reuseIdentifier: reuseIdentifier)
        canShowCallout = false
        displayPriority = .required
        collisionMode = .rectangle
        blur.clipsToBounds = true
        blur.layer.borderColor = UIColor.white.withAlphaComponent(0.15).cgColor
        blur.layer.borderWidth = 0.5
        addSubview(blur)
        icon.tintColor = Theme.uiAccent
        icon.contentMode = .scaleAspectFit
        label.font = .systemFont(ofSize: 12, weight: .semibold)
        label.textColor = .white
        blur.contentView.addSubview(icon)
        blur.contentView.addSubview(label)
        isAccessibilityElement = true
        accessibilityTraits = .button
        accessibilityIdentifier = "worldMapLabel"
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func setText(_ text: String) {
        label.text = text
        accessibilityLabel = "Map \(text)"
        let size = label.sizeThatFits(CGSize(width: 220, height: 40))
        let width = min(size.width, 220)
        let height: CGFloat = 26
        frame = CGRect(x: 0, y: 0, width: width + 38, height: height)
        blur.frame = bounds
        blur.layer.cornerRadius = height / 2
        icon.frame = CGRect(x: 9, y: 6, width: 14, height: 14)
        label.frame = CGRect(x: 28, y: 0, width: width, height: height)
        centerOffset = .zero
    }
}

// MARK: - Terrain overlay

/// Contours and hillshade over Apple's map, drawn by the core in Web Mercator from the DEM (zooms
/// 8–16 shown, DEM ≤ 12; transparent background), so the world view has terrain offline from the
/// tiles on the device. Tiles are made on MapKit's loading threads.
final class TerrainContourTileOverlay: MKTileOverlay {
    private let provider: TerrainProvider
    private let style: ContourRenderRequest.Style
    private let hillshade: Double
    private let zoomCap: Int
    /// Rounds of fetching DEM tiles per overlay tile.
    static let maxRounds = 4

    init(provider: TerrainProvider, dark: Bool, zoomCap: Int) {
        self.provider = provider
        style = dark ? .dark : .paper
        hillshade = dark ? 0.2 : 0.3
        self.zoomCap = zoomCap
        super.init(urlTemplate: nil)
        canReplaceMapContent = false
        minimumZ = 8
        maximumZ = 19
        tileSize = CGSize(width: 256, height: 256)
    }

    override func loadTile(at path: MKTileOverlayPath, result: @escaping (Data?, Error?) -> Void) {
        let provider = self.provider
        let style = self.style
        let hillshade = self.hillshade
        let cap = zoomCap
        let tile = TileID(z: path.z, x: path.x, y: path.y)
        let scale = max(1, Int(path.contentScaleFactor.rounded()))
        Task.detached(priority: .utility) {
            let png = await TerrainContourTileOverlay.render(tile, scale: scale, provider: provider, style: style,
                                                             hillshade: hillshade, cap: cap)
            if let png {
                result(png, nil)
            } else {
                result(nil, NSError(domain: "MapMe.terrain", code: 1))
            }
        }
    }

    /// The overlay tile `tile` as a PNG (nil without a terrain engine or when it failed).
    static func render(_ tile: TileID, scale: Int, provider: TerrainProvider, style: ContourRenderRequest.Style,
                       hillshade: Double, cap: Int) async -> Data? {
        guard let store = provider.store, DEMService.isValid(tile) else { return nil }
        let rect = TileMath.bounds(of: tile)
        let centre = WebMercator.unproject(rect.center)
        let side = 256 * scale
        let metersPerPoint = rect.width / 256 * WebMercator.groundPerMercator(lat: centre.lat)
        let request = ContourRenderRequest(
            space: .mercator, rect: rect.array, style: style, intervalM: nil, scaleMPerPt: metersPerPoint,
            indexEvery: 5, hillshade: hillshade, lineScale: Double(scale), background: [0, 0, 0, 0],
            maxZoom: min(TerrainContourRenderer.demZoom(metersPerPoint: metersPerPoint, latitude: centre.lat, cap: cap), 12),
            minZoom: nil, maxTiles: nil)
        var result = TerrainContourRenderer.render(request, width: side, height: side, store: store)
        var rounds = 0
        while !result.wantsTiles.isEmpty, rounds < maxRounds, !Task.isCancelled {
            rounds += 1
            await provider.load(result.wantsTiles)
            result = TerrainContourRenderer.render(request, width: side, height: side, store: store)
        }
        guard let image = result.image, result.hasTerrain else { return nil }
        return UIImage(cgImage: image).pngData()
    }
}
