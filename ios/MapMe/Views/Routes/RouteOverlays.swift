import QuartzCore
import SwiftUI
import UIKit

// Routes and waypoints on the map canvas (docs/design/explorer.md, Routes: Draw, Waypoints).
// Routes are reprojected once per map (and when the store changes), then drawn like the walked
// trail: paths built in screen space for a zoom bucket (√2 steps) and only moved, turned and
// scaled in between. Waypoints are markers placed per frame, their names placed greedily when the
// zoom or rotation changes.

/// One stretch of a route inside the map's drawable rect, in map pixels.
struct RouteRun: Sendable {
    var pixels: [CGPoint]
    /// Metres along the route at each pixel (splits the followed route into walked and ahead).
    var along: [Double]
}

/// Routes into map pixels: cut where they leave the drawable rect (the crossing found by bisection
/// in latitude/longitude, so a photo's perspective never throws a far point across the map) and
/// simplified (Douglas–Peucker).
enum RouteProjection {
    /// Map pixels: finer than anything drawn (the canvas zooms to a few points per pixel at most).
    static let simplifyTolerance: CGFloat = 0.35
    /// Routes longer than this are projected off the main thread.
    static let backgroundThreshold = 20_000

    static func project(_ points: [LatLon], cumulative: [Double], projector: Georeference.Projector,
                        rect: CGRect) -> [RouteRun] {
        guard points.count == cumulative.count, !points.isEmpty else { return [] }
        func pixel(_ p: LatLon) -> CGPoint? {
            let px = projector.pixel(for: p)
            return px.x.isFinite && px.y.isFinite ? px : nil
        }
        func inside(_ px: CGPoint?) -> Bool {
            guard let px else { return false }
            return rect.contains(px)
        }
        /// The last point still inside going from `a` (inside) toward `b` (outside).
        func crossing(from a: LatLon, _ da: Double, to b: LatLon, _ db: Double) -> (CGPoint, Double)? {
            var lo = 0.0, hi = 1.0
            var best: CGPoint?
            for _ in 0..<12 {
                let t = (lo + hi) / 2
                let q = LatLon(lat: a.lat + (b.lat - a.lat) * t, lon: a.lon + (b.lon - a.lon) * t)
                let px = pixel(q)
                if inside(px) {
                    lo = t
                    best = px
                } else {
                    hi = t
                }
            }
            guard let best else { return nil }
            return (best, da + (db - da) * lo)
        }

        var runs: [RouteRun] = []
        var current = RouteRun(pixels: [], along: [])
        var previousInside = false
        for i in points.indices {
            let px = pixel(points[i])
            let isInside = inside(px)
            if isInside, let px {
                if !previousInside, i > 0,
                   let edge = crossing(from: points[i], cumulative[i], to: points[i - 1], cumulative[i - 1]) {
                    current.pixels.append(edge.0)
                    current.along.append(edge.1)
                }
                current.pixels.append(px)
                current.along.append(cumulative[i])
            } else if previousInside {
                if let edge = crossing(from: points[i - 1], cumulative[i - 1], to: points[i], cumulative[i]) {
                    current.pixels.append(edge.0)
                    current.along.append(edge.1)
                }
                if current.pixels.count >= 2 { runs.append(current) }
                current = RouteRun(pixels: [], along: [])
            }
            previousInside = isInside
        }
        if current.pixels.count >= 2 { runs.append(current) }
        return runs.map { run in
            let keep = RouteGeometry.simplify(run.pixels, tolerance: simplifyTolerance)
            return RouteRun(pixels: keep.map { run.pixels[$0] }, along: keep.map { run.along[$0] })
        }
    }
}

/// Small bitmaps for the routes feature's canvas content, made once and cached. Main thread.
@MainActor
enum RouteArt {
    private static var cache: [String: UIImage] = [:]
    /// Waypoints are red, like the dropped pin and the map's own waypoint markers.
    static let waypointRed = UIColor(red: 1.0, green: 0.23, blue: 0.19, alpha: 1)

    /// A 26 pt red disc with the symbol in white, a white ring and a soft shadow (34 pt when
    /// selected), centred in the image.
    static func waypointMarker(_ symbol: WaypointSymbol, selected: Bool) -> UIImage {
        let key = "wpt|\(symbol.rawValue)|\(selected)"
        if let hit = cache[key] { return hit }
        let d: CGFloat = selected ? 34 : OverlayArt.markerSize
        let pad = OverlayArt.markerPadding
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        let image = UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let cg = ctx.cgContext
            let disc = CGRect(x: pad, y: pad, width: d, height: d)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.4).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: disc).fill()
            cg.restoreGState()
            waypointRed.setFill()
            UIBezierPath(ovalIn: disc.insetBy(dx: 2, dy: 2)).fill()
            drawSymbol(symbol.systemImage, pointSize: d * 0.44, color: .white, in: disc)
        }
        cache[key] = image
        return image
    }

    /// The end of a route: a white disc with a chequered flag.
    static func endFlag() -> UIImage {
        let key = "end"
        if let hit = cache[key] { return hit }
        let d: CGFloat = 22
        let pad: CGFloat = 4
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        let image = UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let rect = CGRect(x: pad, y: pad, width: d, height: d)
            ctx.cgContext.saveGState()
            ctx.cgContext.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.4).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: rect).fill()
            ctx.cgContext.restoreGState()
            drawSymbol("flag.checkered", pointSize: 12, color: .black, in: rect)
        }
        cache[key] = image
        return image
    }

    /// The profile's scrub point on the map: a white-ringed dot in the route's colour.
    static func scrubDot(_ color: UIColor) -> UIImage {
        let d: CGFloat = 16
        let pad: CGFloat = 4
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        return UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let rect = CGRect(x: pad, y: pad, width: d, height: d)
            ctx.cgContext.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.45).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: rect).fill()
            color.setFill()
            UIBezierPath(ovalIn: rect.insetBy(dx: 3, dy: 3)).fill()
        }
    }

    private static func drawSymbol(_ name: String, pointSize: CGFloat, color: UIColor, in rect: CGRect) {
        let config = UIImage.SymbolConfiguration(pointSize: pointSize, weight: .bold)
        guard let symbol = UIImage(systemName: name, withConfiguration: config)?
            .withTintColor(color, renderingMode: .alwaysOriginal) else { return }
        let s = symbol.size
        symbol.draw(in: CGRect(x: rect.midX - s.width / 2, y: rect.midY - s.height / 2, width: s.width, height: s.height))
    }
}

// MARK: - Routes

/// Planned and imported routes on the canvas (under the trail), the followed one highlighted;
/// also the route being edited (`RouteEditTool`) with its handles, and the profile's scrub point.
@MainActor
final class RouteOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.routes
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    /// The map as last told by the canvas, and the last frame drawn: the edit tool converts
    /// touches with them.
    private(set) var mapContext: CanvasMapContext?
    private(set) var lastFrame: CanvasFrame?

    /// Line widths (points): a route, the followed one, the casing around both.
    static let lineWidth: CGFloat = 5
    static let followedWidth: CGFloat = 6
    static let casingWidth: CGFloat = 1.5
    /// Chevrons along the followed route, points apart.
    static let chevronSpacing: CGFloat = 80
    /// A route is hit within this of its line (points)…
    static let hitRadius: CGFloat = 22
    /// …but counts as this much farther than a marker at the same distance.
    static let lineHitPenalty: CGFloat = 8

    private final class Drawn {
        var signature: Int
        var runs: [RouteRun] = []
        let container = CALayer()
        let casing = CAShapeLayer()
        let line = CAShapeLayer()
        let walked = CAShapeLayer()
        let chevrons = CAShapeLayer()
        let start = CALayer()
        let end = CALayer()
        var startPixel: CGPoint?
        var endPixel: CGPoint?
        /// The bucket the paths were built at.
        var builtZoom: CGFloat = 0
        var builtRotation: CGFloat = 0
        var builtOrigin: CGPoint = .zero
        var builtProgress: Double?
        var needsBuild = true
        var color: UIColor = Theme.uiRoute
        var followed = false
        var selected = false

        init(signature: Int) {
            self.signature = signature
        }
    }

    private var drawn: [UUID: Drawn] = [:]
    private var projecting: Set<UUID> = []
    private var mapGeneration = 0
    private var storeLoop: ObservationLoop?
    private var stateLoop: ObservationLoop?
    private var followedID: UUID?
    private var followedProgress: Double?
    private var selectedID: UUID?

    // The route being edited.
    private weak var draftSession: RouteEditSession?
    private let draftCasing = CAShapeLayer()
    private let draftLine = CAShapeLayer()
    private let draftHandles = CAShapeLayer()
    private let draftSelected = CAShapeLayer()
    private var draftPixels: [CGPoint] = []
    private var draftAnchorPixels: [CGPoint] = []
    private var draftSelectedPixel: CGPoint?

    private let scrubLayer = CALayer()
    private var scrubPixel: CGPoint?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        for l in [draftCasing, draftLine] {
            l.fillColor = nil
            l.lineCap = .round
            l.lineJoin = .round
        }
        draftCasing.lineWidth = Self.lineWidth + 2 * Self.casingWidth
        draftLine.lineWidth = Self.lineWidth
        draftLine.strokeColor = Theme.uiRoute.cgColor
        draftLine.lineDashPattern = nil
        // Handles: 10 pt dots in the route's colour with a white ring; the tapped one larger,
        // white with a coloured ring.
        draftHandles.fillColor = Theme.uiRoute.cgColor
        draftHandles.strokeColor = UIColor.white.cgColor
        draftHandles.lineWidth = 2.5
        draftSelected.fillColor = UIColor.white.cgColor
        draftSelected.strokeColor = Theme.uiRoute.cgColor
        draftSelected.lineWidth = 4
        for l in [draftCasing, draftLine, draftHandles, draftSelected] as [CALayer] {
            l.zPosition = 10
            l.isHidden = true
            layer.addSublayer(l)
        }
        scrubLayer.zPosition = 11
        scrubLayer.isHidden = true
        layer.addSublayer(scrubLayer)

        storeLoop = ObservationLoop { [weak self] in
            guard let self, let model = self.model else { return }
            _ = model.geoItems.revision
            self.syncRoutes()
        }
        stateLoop = ObservationLoop { [weak self] in
            guard let self, let model = self.model else { return }
            let navigator = model.navigator
            let followed = navigator.activeRouteID
            let done = navigator.progress?.distanceDoneM
            let selected = self.context?.selectedItem.flatMap { $0.kind == .route ? $0.id : nil }
            let scrub = navigator.screen.scrubPoint
            self.updateState(followed: followed, progress: done, selected: selected, scrub: scrub)
        }
    }

    // MARK: CanvasOverlay

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        mapGeneration += 1
        projecting.removeAll()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for d in drawn.values {
            d.container.removeFromSuperlayer()
            d.start.removeFromSuperlayer()
            d.end.removeFromSuperlayer()
        }
        drawn.removeAll()
        CATransaction.commit()
        syncRoutes()
        draftDidChange()
        updateScrub(model?.navigator.screen.scrubPoint)
    }

    func render(_ frame: CanvasFrame) {
        lastFrame = frame
        let camera = frame.camera
        for d in drawn.values {
            guard !d.runs.isEmpty, !d.container.isHidden else {
                d.start.isHidden = true
                d.end.isHidden = true
                continue
            }
            let ratio = camera.zoom / max(d.builtZoom, 1e-9)
            if d.needsBuild || d.builtZoom <= 0 || ratio > 1.4143 || ratio < 0.7071 {
                build(d, camera: camera)
            }
            let scale = camera.zoom / max(d.builtZoom, 1e-9)
            d.container.position = frame.viewPoint(forMap: d.builtOrigin)
            d.container.setAffineTransform(CGAffineTransform(rotationAngle: camera.rotation - d.builtRotation)
                .scaledBy(x: scale, y: scale))
            // Widths stay constant on screen while the paths scale with the bucket.
            let w = (d.followed || d.selected ? Self.followedWidth : Self.lineWidth) / scale
            d.line.lineWidth = w
            d.walked.lineWidth = w
            d.casing.lineWidth = w + 2 * Self.casingWidth / scale
            d.chevrons.lineWidth = 2 / scale
            if let p = d.startPixel {
                d.start.isHidden = false
                d.start.position = frame.viewPoint(forMap: p)
            } else {
                d.start.isHidden = true
            }
            if let p = d.endPixel {
                d.end.isHidden = false
                d.end.position = frame.viewPoint(forMap: p)
            } else {
                d.end.isHidden = true
            }
        }
        renderDraft(frame)
        if let p = scrubPixel {
            scrubLayer.isHidden = false
            scrubLayer.position = frame.viewPoint(forMap: p)
        } else {
            scrubLayer.isHidden = true
        }
    }

    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit? {
        guard draftSession == nil, frame.camera.zoom > 0 else { return nil }
        let mapPoint = frame.camera.mapPoint(forView: point, viewSize: frame.viewSize)
        let tolerance = Self.hitRadius / frame.camera.zoom
        var best: (id: UUID, d: CGFloat)?
        for (id, d) in drawn where !d.container.isHidden {
            for run in d.runs where run.pixels.count >= 2 {
                for i in 1..<run.pixels.count {
                    let hit = RouteGeometry.distance(from: mapPoint, toSegment: run.pixels[i - 1], run.pixels[i])
                    if hit.distance <= tolerance, hit.distance < (best?.d ?? .infinity) {
                        best = (id: id, d: hit.distance)
                    }
                }
            }
        }
        guard let best else { return nil }
        // A marker on the line (a waypoint, a note) wins over the line itself.
        return CanvasHit(item: GeoItemRef(kind: .route, id: best.id), poi: nil,
                         distance: best.d * frame.camera.zoom + Self.lineHitPenalty)
    }

    // MARK: Routes

    /// What a route is drawn from: its points, kind and colour.
    private static func signature(of route: Route) -> Int {
        var h = Hasher()
        h.combine(route.geometryHash)
        h.combine(route.kind)
        h.combine(route.displayColor)
        return h.finalize()
    }

    /// Adds, updates and removes routes after the store changed.
    private func syncRoutes() {
        guard let model, let ctx = mapContext, let projector = ctx.projector else { return }
        let routes = model.geoItems.routes
        let ids = Set(routes.map(\.id))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (id, d) in drawn where !ids.contains(id) {
            d.container.removeFromSuperlayer()
            d.start.removeFromSuperlayer()
            d.end.removeFromSuperlayer()
            drawn[id] = nil
        }
        CATransaction.commit()
        let rect = ctx.drawableRect
        let generation = mapGeneration
        for route in routes {
            let signature = Self.signature(of: route)
            if let d = drawn[route.id], d.signature == signature { continue }
            if projecting.contains(route.id) { continue }
            let points = route.latLons
            let cumulative = RouteGeometry.cumulativeDistances(points)
            let color = route.displayColor.uiColor
            let id = route.id
            if points.count > RouteProjection.backgroundThreshold {
                projecting.insert(id)
                Task { [weak self] in
                    let runs = await Task.detached(priority: .userInitiated) {
                        RouteProjection.project(points, cumulative: cumulative, projector: projector, rect: rect)
                    }.value
                    guard let self, self.mapGeneration == generation else { return }
                    self.projecting.remove(id)
                    self.install(id: id, signature: signature, runs: runs, color: color, points: points)
                }
            } else {
                let runs = RouteProjection.project(points, cumulative: cumulative, projector: projector, rect: rect)
                install(id: id, signature: signature, runs: runs, color: color, points: points)
            }
        }
    }

    private func install(id: UUID, signature: Int, runs: [RouteRun], color: UIColor, points: [LatLon]) {
        guard let ctx = mapContext else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        let d: Drawn
        if let existing = drawn[id] {
            d = existing
            d.signature = signature
        } else {
            d = Drawn(signature: signature)
            for l in [d.casing, d.line, d.walked] {
                l.fillColor = nil
                l.lineCap = .round
                l.lineJoin = .round
            }
            d.walked.opacity = 0.45
            d.chevrons.fillColor = nil
            d.chevrons.strokeColor = UIColor.white.withAlphaComponent(0.92).cgColor
            d.chevrons.lineCap = .round
            d.chevrons.lineJoin = .round
            for l in [d.casing, d.walked, d.line, d.chevrons] { d.container.addSublayer(l) }
            let startImage = OverlayArt.trailStart()
            d.start.contents = startImage.cgImage
            d.start.contentsScale = startImage.scale
            d.start.bounds = CGRect(origin: .zero, size: startImage.size)
            let endImage = RouteArt.endFlag()
            d.end.contents = endImage.cgImage
            d.end.contentsScale = endImage.scale
            d.end.bounds = CGRect(origin: .zero, size: endImage.size)
            layer.addSublayer(d.container)
            layer.addSublayer(d.start)
            layer.addSublayer(d.end)
            drawn[id] = d
        }
        d.runs = runs
        d.color = color
        d.casing.strokeColor = (ctx.darkMap ? UIColor.black.withAlphaComponent(0.35) : UIColor.white.withAlphaComponent(0.9)).cgColor
        d.line.strokeColor = color.cgColor
        d.walked.strokeColor = color.cgColor
        // The route's own ends, when they are on this map.
        d.startPixel = points.first.flatMap { ctx.pixel(for: $0) }.flatMap { ctx.drawableRect.contains($0) ? $0 : nil }
        d.endPixel = points.last.flatMap { ctx.pixel(for: $0) }.flatMap { ctx.drawableRect.contains($0) ? $0 : nil }
        d.needsBuild = true
        applyState(to: d, id: id)
        ctx.requestFrame()
    }

    private func updateState(followed: UUID?, progress: Double?, selected: UUID?, scrub: LatLon?) {
        let changed = followed != followedID || selected != selectedID
        let moved = progress != followedProgress
        followedID = followed
        followedProgress = progress
        selectedID = selected
        if changed {
            for (id, d) in drawn { applyState(to: d, id: id) }
        } else if moved, let id = followed, let d = drawn[id] {
            d.needsBuild = true
        }
        updateScrub(scrub)
        mapContext?.requestFrame()
    }

    private func applyState(to d: Drawn, id: UUID) {
        let followed = id == followedID
        let selected = id == selectedID
        if d.followed != followed || d.selected != selected { d.needsBuild = true }
        d.followed = followed
        d.selected = selected
        d.container.zPosition = followed ? 3 : (selected ? 2 : 0)
        d.start.zPosition = d.container.zPosition + 0.5
        d.end.zPosition = d.container.zPosition + 0.5
        d.chevrons.isHidden = !followed
        d.walked.isHidden = !followed
        // The route being edited shows as the draft instead.
        let editing = draftSession?.routeID == id
        d.container.isHidden = editing
        d.start.isHidden = editing
        d.end.isHidden = editing
    }

    /// Builds a route's paths in screen space for the camera's zoom bucket.
    private func build(_ d: Drawn, camera: CanvasCamera) {
        d.needsBuild = false
        d.builtZoom = camera.zoom
        d.builtRotation = camera.rotation
        d.builtOrigin = camera.center
        let c = cos(camera.rotation), s = sin(camera.rotation)
        let zoom = camera.zoom
        let origin = camera.center
        func local(_ p: CGPoint) -> CGPoint {
            let dx = (p.x - origin.x) * zoom, dy = (p.y - origin.y) * zoom
            return CGPoint(x: dx * c - dy * s, y: dx * s + dy * c)
        }
        let split = d.followed ? followedProgress : nil
        d.builtProgress = split
        let ahead = CGMutablePath()
        let behind = CGMutablePath()
        var chevronPoints: [(CGPoint, CGPoint)] = []
        var sinceChevron: CGFloat = Self.chevronSpacing / 2
        for run in d.runs {
            var last: CGPoint?
            var lastAlong = 0.0
            var aheadOpen = false
            var behindOpen = false
            for (i, p) in run.pixels.enumerated() {
                let v = local(p)
                let along = run.along[i]
                if let l = last {
                    // Skip points closer than half a point (the path stays light); keep run ends.
                    if hypot(v.x - l.x, v.y - l.y) < 0.5, i < run.pixels.count - 1 { continue }
                    if let split, lastAlong < split, along > split {
                        // The segment crosses where you are: split it there.
                        let t = CGFloat((split - lastAlong) / (along - lastAlong))
                        let m = CGPoint(x: l.x + (v.x - l.x) * t, y: l.y + (v.y - l.y) * t)
                        behind.addLine(to: m)
                        ahead.move(to: m)
                        aheadOpen = true
                        ahead.addLine(to: v)
                        addChevrons(from: m, to: v, since: &sinceChevron, into: &chevronPoints)
                    } else if let split, along <= split {
                        if !behindOpen {
                            behind.move(to: l)
                            behindOpen = true
                        }
                        behind.addLine(to: v)
                    } else {
                        if !aheadOpen {
                            ahead.move(to: l)
                            aheadOpen = true
                        }
                        ahead.addLine(to: v)
                        if d.followed { addChevrons(from: l, to: v, since: &sinceChevron, into: &chevronPoints) }
                    }
                } else if let split, along <= split {
                    behind.move(to: v)
                    behindOpen = true
                } else {
                    ahead.move(to: v)
                    aheadOpen = true
                }
                last = v
                lastAlong = along
            }
        }
        let chevrons = CGMutablePath()
        for (p, dir) in chevronPoints {
            // A small "›" pointing along the route.
            let back = CGPoint(x: -dir.x * 4, y: -dir.y * 4)
            let side = CGPoint(x: -dir.y * 4, y: dir.x * 4)
            chevrons.move(to: CGPoint(x: p.x + back.x + side.x, y: p.y + back.y + side.y))
            chevrons.addLine(to: CGPoint(x: p.x + dir.x * 2, y: p.y + dir.y * 2))
            chevrons.addLine(to: CGPoint(x: p.x + back.x - side.x, y: p.y + back.y - side.y))
        }
        let all = CGMutablePath()
        all.addPath(behind)
        all.addPath(ahead)
        d.casing.path = all
        d.line.path = ahead
        d.walked.path = behind
        d.chevrons.path = chevrons
    }

    /// Chevrons every `chevronSpacing` points along a segment (screen space).
    private func addChevrons(from a: CGPoint, to b: CGPoint, since: inout CGFloat, into out: inout [(CGPoint, CGPoint)]) {
        let len = hypot(b.x - a.x, b.y - a.y)
        guard len > 0 else { return }
        let dir = CGPoint(x: (b.x - a.x) / len, y: (b.y - a.y) / len)
        var at = Self.chevronSpacing - since
        while at <= len {
            out.append((CGPoint(x: a.x + dir.x * at, y: a.y + dir.y * at), dir))
            at += Self.chevronSpacing
        }
        since = len - (at - Self.chevronSpacing)
    }

    // MARK: Draft

    /// Shows `session`'s route while it is edited (nil: none).
    func setDraft(_ session: RouteEditSession?) {
        draftSession = session
        for (id, d) in drawn { applyState(to: d, id: id) }
        draftDidChange()
    }

    /// The edited route changed: reproject it (small: projected on the main thread).
    func draftDidChange() {
        guard let session = draftSession, let ctx = mapContext else {
            draftPixels = []
            draftAnchorPixels = []
            draftSelectedPixel = nil
            mapContext?.requestFrame()
            return
        }
        draftPixels = session.points.compactMap { ctx.pixel(for: $0.latLon) }
        draftAnchorPixels = session.points.filter(\.isAnchor).compactMap { ctx.pixel(for: $0.latLon) }
        draftSelectedPixel = session.selectedAnchor.flatMap { i in
            i < session.points.count ? ctx.pixel(for: session.points[i].latLon) : nil
        }
        ctx.requestFrame()
    }

    private func renderDraft(_ frame: CanvasFrame) {
        guard let session = draftSession else {
            for l in [draftCasing, draftLine, draftHandles, draftSelected] { l.isHidden = true }
            return
        }
        let line = CGMutablePath()
        var first = true
        for p in draftPixels {
            let v = frame.viewPoint(forMap: p)
            if first { line.move(to: v) } else { line.addLine(to: v) }
            first = false
        }
        // The stroke being traced, live (kept in map pixels, like the points).
        for p in session.liveTrace {
            let v = frame.viewPoint(forMap: p)
            if first { line.move(to: v) } else { line.addLine(to: v) }
            first = false
        }
        draftCasing.strokeColor = ((mapContext?.darkMap ?? true) ? UIColor.black.withAlphaComponent(0.35)
            : UIColor.white.withAlphaComponent(0.9)).cgColor
        draftCasing.path = line
        draftLine.path = line
        let handles = CGMutablePath()
        for p in draftAnchorPixels {
            let v = frame.viewPoint(forMap: p)
            handles.addEllipse(in: CGRect(x: v.x - 5, y: v.y - 5, width: 10, height: 10))
        }
        draftHandles.path = handles
        if let p = draftSelectedPixel {
            let v = frame.viewPoint(forMap: p)
            draftSelected.path = CGPath(ellipseIn: CGRect(x: v.x - 9, y: v.y - 9, width: 18, height: 18), transform: nil)
            draftSelected.isHidden = false
        } else {
            draftSelected.isHidden = true
        }
        for l in [draftCasing, draftLine, draftHandles] { l.isHidden = false }
    }

    // MARK: Scrub

    private func updateScrub(_ point: LatLon?) {
        guard let point, let ctx = mapContext, let px = ctx.pixel(for: point), ctx.drawableRect.contains(px) else {
            scrubPixel = nil
            return
        }
        if scrubLayer.contents == nil {
            let image = RouteArt.scrubDot(Theme.uiRoute)
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            scrubLayer.contents = image.cgImage
            scrubLayer.contentsScale = image.scale
            scrubLayer.bounds = CGRect(origin: .zero, size: image.size)
            CATransaction.commit()
        }
        scrubPixel = px
    }

    // MARK: Edit tool support

    /// A map pixel of the current map → its coordinate.
    func latLon(forPixel p: CGPoint) -> LatLon? {
        guard let ll = mapContext?.latLon(forPixel: p), ll.lat.isFinite, ll.lon.isFinite else { return nil }
        return ll
    }

    /// A coordinate → its view point in the last frame.
    func viewPoint(for p: LatLon) -> CGPoint? {
        guard let frame = lastFrame, let px = mapContext?.pixel(for: p) else { return nil }
        return frame.viewPoint(forMap: px)
    }
}

// MARK: - Waypoints

/// Waypoints on the canvas (above the map's own POIs): red markers with the waypoint's symbol,
/// names as halo labels when zoomed in (≤ 7 m per point), placed greedily among themselves.
@MainActor
final class WaypointOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.markers
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    /// Names show at this ground scale and closer (metres per point).
    static let labelMaxMetersPerPoint = 7.0
    static let hitRadius: CGFloat = 22

    private struct Item {
        let id: UUID
        let pixel: CGPoint
        let marker: CALayer
        let label: CALayer
        let labelSize: CGSize
        var labelShown = false
    }

    private var items: [Item] = []
    private var mapContext: CanvasMapContext?
    private var loop: ObservationLoop?
    private var selectedID: UUID?
    /// The camera the labels were placed for.
    private var placedZoom: CGFloat = 0
    private var placedRotation: CGFloat = 0
    private var placedLabels = false
    private var needsPlacement = true

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        loop = ObservationLoop { [weak self] in
            guard let self, let model = self.model else { return }
            _ = model.geoItems.revision
            let selected = self.context?.selectedItem.flatMap { $0.kind == .waypoint ? $0.id : nil }
            self.selectedID = selected
            self.rebuild()
        }
    }

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        rebuild()
    }

    private func rebuild() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        for item in items {
            item.marker.removeFromSuperlayer()
            item.label.removeFromSuperlayer()
        }
        items.removeAll()
        guard let model, let ctx = mapContext, ctx.projector != nil else { return }
        let rect = ctx.drawableRect
        let halo = ctx.darkMap ? UIColor.black.withAlphaComponent(0.85) : UIColor.white.withAlphaComponent(0.92)
        let ink = ctx.darkMap ? UIColor.white : UIColor.black
        // Selected last, so it is on top.
        let waypoints = model.geoItems.waypoints.sorted { a, b in (a.id == selectedID ? 1 : 0) < (b.id == selectedID ? 1 : 0) }
        for w in waypoints {
            guard let px = ctx.pixel(for: w.position), rect.contains(px) else { continue }
            let selected = w.id == selectedID
            let image = RouteArt.waypointMarker(w.symbol, selected: selected)
            let marker = CALayer()
            marker.contents = image.cgImage
            marker.contentsScale = image.scale
            marker.bounds = CGRect(origin: .zero, size: image.size)
            let text = OverlayArt.haloText(w.name, color: ink, halo: halo, fontSize: 12, weight: .semibold)
            let label = CALayer()
            label.contents = text.cgImage
            label.contentsScale = text.scale
            label.bounds = CGRect(origin: .zero, size: text.size)
            label.anchorPoint = CGPoint(x: 0, y: 0.5)
            label.isHidden = true
            layer.addSublayer(marker)
            layer.addSublayer(label)
            items.append(Item(id: w.id, pixel: px, marker: marker, label: label, labelSize: text.size))
        }
        needsPlacement = true
        ctx.requestFrame()
    }

    func render(_ frame: CanvasFrame) {
        guard !items.isEmpty else { return }
        let showLabels = frame.metersPerPoint > 0 && frame.metersPerPoint <= Self.labelMaxMetersPerPoint
        let camera = frame.camera
        if needsPlacement || showLabels != placedLabels || abs(camera.zoom / max(placedZoom, 1e-9) - 1) > 0.02
            || abs(camera.rotation - placedRotation) > 0.01 {
            place(frame: frame, showLabels: showLabels)
        }
        for item in items {
            let v = frame.viewPoint(forMap: item.pixel)
            item.marker.position = v
            if item.labelShown {
                item.label.position = CGPoint(x: v.x + OverlayArt.markerSize / 2 + 4, y: v.y)
            }
        }
    }

    /// Greedy label placement: the selected waypoint's first, then the others; a label that would
    /// overlap a marker or a label placed before stays hidden.
    private func place(frame: CanvasFrame, showLabels: Bool) {
        needsPlacement = false
        placedLabels = showLabels
        placedZoom = frame.camera.zoom
        placedRotation = frame.camera.rotation
        var taken: [CGRect] = items.map { item in
            let v = frame.viewPoint(forMap: item.pixel)
            let r = OverlayArt.markerSize / 2
            return CGRect(x: v.x - r, y: v.y - r, width: 2 * r, height: 2 * r)
        }
        for i in items.indices.reversed() {
            guard showLabels else {
                items[i].labelShown = false
                items[i].label.isHidden = true
                continue
            }
            let v = frame.viewPoint(forMap: items[i].pixel)
            let size = items[i].labelSize
            let rect = CGRect(x: v.x + OverlayArt.markerSize / 2 + 4, y: v.y - size.height / 2,
                              width: size.width, height: size.height)
            // Its own marker is left of the label; any other marker or label placed before blocks it.
            let free = !taken.contains { $0.intersects(rect) }
            items[i].labelShown = free
            items[i].label.isHidden = !free
            if free { taken.append(rect) }
        }
    }

    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit? {
        var best: (id: UUID, d: CGFloat)?
        for item in items {
            let v = frame.viewPoint(forMap: item.pixel)
            let d = hypot(v.x - point.x, v.y - point.y)
            if d <= Self.hitRadius, d < (best?.d ?? .infinity) { best = (id: item.id, d: d) }
        }
        guard let best else { return nil }
        return CanvasHit(item: GeoItemRef(kind: .waypoint, id: best.id), poi: nil, distance: best.d)
    }
}
