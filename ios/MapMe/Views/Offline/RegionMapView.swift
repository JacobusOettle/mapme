import MapKit
import SwiftUI
import UIKit

/// The region picker's map (docs/design/explorer.md, Pick a region): Apple's dark muted map with
/// the rectangle to download drawn over it (the rest dimmed), four 44 pt corner handles to adjust
/// it, and the regions already downloaded as dashed outlines. The rectangle pans and zooms with
/// the map; a handle drag moves that corner (the opposite one stays). The map never rotates or
/// tilts, so a latitude/longitude box stays a screen rectangle.
struct RegionMapView: UIViewRepresentable {
    @Binding var bounds: GeoBounds?
    /// Bump to fit the map to `bounds` (a search result, a preset).
    var fitRequest: Int
    /// Regions on the device, drawn dashed.
    var existing: [GeoBounds]
    /// The visible area (for "This view").
    var onVisibleRegion: (GeoBounds) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> RegionMapContainer {
        let container = RegionMapContainer()
        container.mapView.delegate = context.coordinator
        container.selection.onChange = { [weak coordinator = context.coordinator] newBounds, ended in
            coordinator?.selectionChanged(newBounds, ended: ended)
        }
        context.coordinator.container = container
        return container
    }

    func updateUIView(_ container: RegionMapContainer, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        if !coordinator.isDragging {
            container.selection.box = bounds
        }
        if coordinator.lastExisting != existing {
            coordinator.lastExisting = existing
            container.setExisting(existing)
        }
        if coordinator.lastFit != fitRequest {
            coordinator.lastFit = fitRequest
            if let bounds { container.fit(bounds, animated: coordinator.hasFitted) }
            coordinator.hasFitted = true
        }
        container.selection.relayout()
    }

    final class Coordinator: NSObject, MKMapViewDelegate {
        var parent: RegionMapView
        weak var container: RegionMapContainer?
        var isDragging = false
        var lastFit = -1
        var hasFitted = false
        var lastExisting: [GeoBounds] = []

        init(_ parent: RegionMapView) {
            self.parent = parent
        }

        func selectionChanged(_ newBounds: GeoBounds, ended: Bool) {
            isDragging = !ended
            parent.bounds = newBounds
        }

        func mapViewDidChangeVisibleRegion(_ mapView: MKMapView) {
            container?.selection.relayout()
        }

        func mapView(_ mapView: MKMapView, regionDidChangeAnimated animated: Bool) {
            container?.selection.relayout()
            let region = mapView.region
            let visible = GeoBounds(south: max(-85, region.center.latitude - region.span.latitudeDelta / 2),
                                    west: max(-180, region.center.longitude - region.span.longitudeDelta / 2),
                                    north: min(85, region.center.latitude + region.span.latitudeDelta / 2),
                                    east: min(180, region.center.longitude + region.span.longitudeDelta / 2))
            let report = parent.onVisibleRegion
            // Not while SwiftUI is updating the view (a fit moves the map synchronously).
            DispatchQueue.main.async { report(visible) }
        }

        func mapView(_ mapView: MKMapView, rendererFor overlay: MKOverlay) -> MKOverlayRenderer {
            guard let polygon = overlay as? MKPolygon else { return MKOverlayRenderer(overlay: overlay) }
            let renderer = MKPolygonRenderer(polygon: polygon)
            renderer.strokeColor = UIColor.white.withAlphaComponent(0.55)
            renderer.fillColor = Theme.uiAccent.withAlphaComponent(0.06)
            renderer.lineWidth = 1.5
            renderer.lineDashPattern = [6, 4]
            return renderer
        }
    }
}

/// The map and the selection drawn over it.
final class RegionMapContainer: UIView {
    let mapView = MKMapView()
    let selection = RegionSelectionOverlay()

    override init(frame: CGRect) {
        super.init(frame: frame)
        let config = MKStandardMapConfiguration(elevationStyle: .realistic, emphasisStyle: .muted)
        config.pointOfInterestFilter = .excludingAll
        mapView.preferredConfiguration = config
        mapView.overrideUserInterfaceStyle = .dark
        mapView.showsUserLocation = true
        mapView.isRotateEnabled = false
        mapView.isPitchEnabled = false
        mapView.showsCompass = false
        mapView.accessibilityIdentifier = "regionMapView"
        addSubview(mapView)
        selection.mapView = mapView
        addSubview(selection)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override func layoutSubviews() {
        super.layoutSubviews()
        mapView.frame = bounds
        selection.frame = bounds
        selection.relayout()
    }

    /// Shows `geo` with room for the search bar above and the panel below.
    func fit(_ geo: GeoBounds, animated: Bool) {
        let rect = Self.mapRect(geo)
        let padding = UIEdgeInsets(top: 170, left: 36, bottom: 330, right: 36)
        mapView.setVisibleMapRect(rect, edgePadding: padding, animated: animated)
    }

    func setExisting(_ boxes: [GeoBounds]) {
        mapView.removeOverlays(mapView.overlays)
        let polygons = boxes.map { box -> MKPolygon in
            var corners = [CLLocationCoordinate2D(latitude: box.north, longitude: box.west),
                           CLLocationCoordinate2D(latitude: box.north, longitude: box.east),
                           CLLocationCoordinate2D(latitude: box.south, longitude: box.east),
                           CLLocationCoordinate2D(latitude: box.south, longitude: box.west)]
            return MKPolygon(coordinates: &corners, count: corners.count)
        }
        mapView.addOverlays(polygons)
    }

    /// The box in MapKit's map points (across the antimeridian it runs past the world's width).
    static func mapRect(_ geo: GeoBounds) -> MKMapRect {
        let nw = MKMapPoint(CLLocationCoordinate2D(latitude: geo.north, longitude: geo.west))
        let se = MKMapPoint(CLLocationCoordinate2D(latitude: geo.south, longitude: geo.east))
        var width = se.x - nw.x
        if width <= 0 { width += MKMapSize.world.width }
        return MKMapRect(x: nw.x, y: nw.y, width: width, height: max(se.y - nw.y, 1))
    }
}

/// The rectangle being picked: everything outside it dimmed, an accent outline, four corner
/// handles. Touches outside the handles go to the map.
final class RegionSelectionOverlay: UIView {
    weak var mapView: MKMapView?
    /// The box changed (`ended`: the drag finished).
    var onChange: ((GeoBounds, Bool) -> Void)?

    /// Smallest side, degrees (~1 km).
    static let minimumSpan = 0.01

    private let dim = CAShapeLayer()
    private let outline = CAShapeLayer()
    private var handles: [UIView] = []
    /// The opposite corner while a handle is dragged.
    private var anchor: CLLocationCoordinate2D?

    /// The rectangle (nil: none yet).
    var box: GeoBounds? {
        didSet {
            if box != oldValue { relayout() }
        }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        dim.fillRule = .evenOdd
        dim.fillColor = UIColor.black.withAlphaComponent(0.38).cgColor
        layer.addSublayer(dim)
        outline.fillColor = UIColor.clear.cgColor
        outline.strokeColor = Theme.uiAccent.cgColor
        outline.lineWidth = 2.5
        outline.shadowColor = UIColor.black.cgColor
        outline.shadowOpacity = 0.5
        outline.shadowRadius = 3
        outline.shadowOffset = .zero
        layer.addSublayer(outline)
        let labels = ["Top-left corner", "Top-right corner", "Bottom-right corner", "Bottom-left corner"]
        for i in 0..<4 {
            let handle = UIView(frame: CGRect(x: 0, y: 0, width: Theme.minTapTarget, height: Theme.minTapTarget))
            handle.backgroundColor = .clear
            let knob = UIView(frame: CGRect(x: 11, y: 11, width: 22, height: 22))
            knob.backgroundColor = .white
            knob.layer.cornerRadius = 11
            knob.layer.borderColor = Theme.uiAccent.cgColor
            knob.layer.borderWidth = 3
            knob.layer.shadowColor = UIColor.black.cgColor
            knob.layer.shadowOpacity = 0.45
            knob.layer.shadowRadius = 3
            knob.layer.shadowOffset = CGSize(width: 0, height: 1)
            knob.isUserInteractionEnabled = false
            handle.addSubview(knob)
            handle.isAccessibilityElement = true
            handle.accessibilityLabel = labels[i]
            handle.accessibilityHint = "Drag to change the area to download"
            handle.accessibilityIdentifier = "regionHandle-\(i)"
            handle.tag = i
            handle.addGestureRecognizer(UIPanGestureRecognizer(target: self, action: #selector(drag(_:))))
            addSubview(handle)
            handles.append(handle)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    /// Only the handles take touches; the map gets the rest.
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        handles.contains { !$0.isHidden && $0.frame.contains(point) }
    }

    /// The box on screen now (nil without a box or a map).
    private var screenRect: CGRect? {
        guard let mapView, let box, mapView.bounds.width > 0 else { return nil }
        let rect = RegionMapContainer.mapRect(box)
        let region = MKCoordinateRegion(rect)
        let r = mapView.convert(region, toRectTo: self)
        guard r.width.isFinite, r.height.isFinite else { return nil }
        return r.standardized
    }

    func relayout() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        dim.frame = layer.bounds
        outline.frame = layer.bounds
        guard let r = screenRect else {
            dim.path = nil
            outline.path = nil
            handles.forEach { $0.isHidden = true }
            return
        }
        let path = UIBezierPath(rect: layer.bounds.insetBy(dx: -4_000, dy: -4_000))
        path.append(UIBezierPath(rect: r))
        dim.path = path.cgPath
        outline.path = UIBezierPath(rect: r).cgPath
        let corners = [CGPoint(x: r.minX, y: r.minY), CGPoint(x: r.maxX, y: r.minY),
                       CGPoint(x: r.maxX, y: r.maxY), CGPoint(x: r.minX, y: r.maxY)]
        for (handle, corner) in zip(handles, corners) {
            handle.isHidden = false
            handle.center = corner
        }
    }

    @objc private func drag(_ gesture: UIPanGestureRecognizer) {
        guard let mapView, let handle = gesture.view, let box else { return }
        let i = handle.tag
        switch gesture.state {
        case .began:
            // The opposite corner stays put.
            let opposite = [(box.south, box.east), (box.south, box.west), (box.north, box.west), (box.north, box.east)][i]
            anchor = CLLocationCoordinate2D(latitude: opposite.0, longitude: opposite.1)
            UISelectionFeedbackGenerator().selectionChanged()
        case .changed, .ended:
            guard let anchor else { return }
            let point = gesture.location(in: self)
            let c = mapView.convert(point, toCoordinateFrom: self)
            guard CLLocationCoordinate2DIsValid(c) else { return }
            let south = max(-85, min(c.latitude, anchor.latitude))
            let north = min(85, max(c.latitude, anchor.latitude))
            let west = min(c.longitude, anchor.longitude)
            let east = max(c.longitude, anchor.longitude)
            var next = GeoBounds(south: south, west: west, north: north, east: east)
            if next.north - next.south < Self.minimumSpan {
                if c.latitude < anchor.latitude { next.south = next.north - Self.minimumSpan } else { next.north = next.south + Self.minimumSpan }
            }
            if next.east - next.west < Self.minimumSpan {
                if c.longitude < anchor.longitude { next.west = next.east - Self.minimumSpan } else { next.east = next.west + Self.minimumSpan }
            }
            self.box = next
            let ended = gesture.state == .ended
            if ended { self.anchor = nil }
            onChange?(next, ended)
        case .cancelled, .failed:
            anchor = nil
            if let current = box { onChange?(current, true) }
        default:
            break
        }
    }
}
