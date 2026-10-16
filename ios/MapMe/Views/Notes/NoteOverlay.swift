import QuartzCore
import UIKit

/// Notes and photo thumbnails on the canvas, above the map's own POIs and under the puck
/// (docs/design/explorer.md, Notes and photos → Draw):
///
/// * photo notes as 34 pt round thumbnails (white ring, shadow), text notes as 26 pt amber
///   markers;
/// * pins within 40 pt of each other collapse into a stack with a count badge (the selected note
///   always stays on top of its stack);
/// * zoomed out past 15 m/pt they become 10 pt dots.
///
/// Notes are global: every map shows those inside its drawable rect (the sheet and the terrain
/// beyond it). Positions are projected once per map (`mapDidChange`, with the warp); stacks are
/// worked out again only when the zoom or the rotation changes; `render` only moves layers.
@MainActor
final class NoteOverlay: CanvasOverlay {
    let level = CanvasOverlayLevel.markers
    let layer = CALayer()
    private weak var model: AppModel?
    private weak var context: MapScreenContext?

    /// Pins collapse into a stack within this distance (points).
    static let stackRadius: CGFloat = 40
    /// Dots when zoomed out past this many ground metres per point…
    static let dotMetersPerPoint = 15.0
    /// …which merge within this distance.
    static let dotMergeRadius: CGFloat = 8
    /// The selected note's pin is drawn this much larger.
    static let selectedScale: CGFloat = 1.3

    @MainActor
    private final class Item {
        let note: MapNote
        let pixel: CGPoint
        /// Moved per frame; holds the stack's back card, the pin and the count badge, in that
        /// order (a layer's sublayers always draw over its own contents).
        let node = CALayer()
        let stackBack = CALayer()
        let pin = CALayer()
        let badge = CALayer()
        var hasThumbnail = false
        var isSelected = false
        /// Leads its stack (drawn); the others of the stack are hidden.
        var isLeader = true
        var count = 1
        var isDot = false

        init(note: MapNote, pixel: CGPoint) {
            self.note = note
            self.pixel = pixel
            node.isHidden = true
            stackBack.isHidden = true
            badge.isHidden = true
            node.addSublayer(stackBack)
            node.addSublayer(pin)
            node.addSublayer(badge)
        }

        /// Half the drawn pin, points (for hits).
        var radius: CGFloat {
            if isDot { return NoteArt.dotSize / 2 }
            let side = note.photo != nil ? NoteArt.photoSize : NoteArt.textSize
            return side / 2 * (isSelected ? NoteOverlay.selectedScale : 1)
        }

        /// Larger while selected (not as a dot).
        func applyScale() {
            let s = NoteOverlay.selectedScale
            node.setAffineTransform(isSelected && !isDot ? CGAffineTransform(scaleX: s, y: s) : .identity)
        }
    }

    private var items: [Item] = []
    /// The latest notes and selection (kept for a map change).
    private var notes: [MapNote] = []
    private var selectedID: UUID?
    private var mapContext: CanvasMapContext?
    /// Coordinates around the drawable rect: notes outside it are not projected at all.
    private var area: GeoBounds?
    private var observation: ObservationLoop?
    /// The zoom, rotation and style the stacks were worked out for (nil: work them out).
    private var placement: (zoom: CGFloat, rotation: CGFloat, dots: Bool)?

    init(model: AppModel, context: MapScreenContext) {
        self.model = model
        self.context = context
        observation = ObservationLoop { [weak self] in self?.reload() }
    }

    // MARK: - Data

    /// Reads the notes, the thumbnails and the selection (observed: runs again when they change).
    private func reload() {
        guard let model else { return }
        notes = model.geoItems.notes
        let ref = context?.selectedItem
        selectedID = ref?.kind == .note ? ref?.id : nil
        rebuild()
    }

    func mapDidChange(_ context: CanvasMapContext) {
        mapContext = context
        area = Self.area(of: context)
        rebuild()
    }

    /// The coordinates around the drawable rect (its corners and edge midpoints, grown by 10 %): a
    /// quick test before projecting a note through the georeference. Should the enlarged rect not
    /// map to coordinates, the sheet's does, grown by half its size.
    private static func area(of context: CanvasMapContext) -> GeoBounds? {
        guard context.georef != nil else { return nil }
        return bounds(of: context.drawableRect, context: context, grow: 0.1)
            ?? bounds(of: context.sheetRect, context: context, grow: 0.5)
    }

    private static func bounds(of r: CGRect, context: CanvasMapContext, grow: Double) -> GeoBounds? {
        let points = [CGPoint(x: r.minX, y: r.minY), CGPoint(x: r.midX, y: r.minY), CGPoint(x: r.maxX, y: r.minY),
                      CGPoint(x: r.maxX, y: r.midY), CGPoint(x: r.maxX, y: r.maxY), CGPoint(x: r.midX, y: r.maxY),
                      CGPoint(x: r.minX, y: r.maxY), CGPoint(x: r.minX, y: r.midY)]
        let corners = points.compactMap { context.latLon(forPixel: $0) }
            .filter { $0.lat.isFinite && $0.lon.isFinite && abs($0.lat) <= 90 }
        guard corners.count == points.count, let bounds = GeoBounds.enclosing(corners) else { return nil }
        let diagonal = Geodesy.distance(LatLon(lat: bounds.south, lon: bounds.west), LatLon(lat: bounds.north, lon: bounds.east))
        return bounds.expanded(byMeters: max(diagonal, 100) * grow)
    }

    /// Makes the pins for the notes on this map, reusing the layers of unchanged ones.
    private func rebuild() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        guard let mapContext, let area else {
            for item in items { item.node.removeFromSuperlayer() }
            items = []
            return
        }
        let drawable = mapContext.drawableRect
        var old: [UUID: Item] = [:]
        for item in items { old[item.note.id] = item }
        var next: [Item] = []
        for note in notes where area.contains(note.position) {
            guard let pixel = mapContext.pixel(for: note.position), drawable.contains(pixel) else { continue }
            let existing = old.removeValue(forKey: note.id)
            let item: Item
            if let existing, existing.note == note, existing.pixel == pixel {
                item = existing
            } else {
                existing?.node.removeFromSuperlayer()
                item = Item(note: note, pixel: pixel)
                applyArt(item)
                layer.addSublayer(item.node)
            }
            item.isSelected = note.id == selectedID
            next.append(item)
        }
        for item in old.values { item.node.removeFromSuperlayer() }
        items = next
        // Newest on top (the store keeps notes newest first), the selected one above all.
        for (i, item) in items.enumerated() {
            item.node.zPosition = item.isSelected ? 1_000_000 : CGFloat(items.count - i)
            item.applyScale()
            // A thumbnail that has loaded since: the pin shows the photo now.
            if let photo = item.note.photo, !item.hasThumbnail, let thumb = model?.notes.thumbnail(for: photo) {
                item.hasThumbnail = true
                if !item.isDot { setImage(NoteArt.photoPin(thumb, id: photo.id), on: item) }
            }
        }
        placement = nil
        // On the next turn: a frame drawn now (the canvas draws at once when it is off screen)
        // would run inside the observation of `reload` and be tracked with it.
        let requestFrame = mapContext.requestFrame
        Task { @MainActor in requestFrame() }
    }

    /// The pin's images for its kind and style (dot or pin, thumbnail when loaded).
    private func applyArt(_ item: Item) {
        if item.isDot {
            setImage(NoteArt.dot(), on: item)
            return
        }
        if let photo = item.note.photo {
            let thumb = model?.notes.thumbnail(for: photo)
            item.hasThumbnail = thumb != nil
            setImage(NoteArt.photoPin(thumb, id: photo.id), on: item)
        } else {
            setImage(NoteArt.textPin(), on: item)
        }
        let back = NoteArt.stackBack(photo: item.note.photo != nil)
        item.stackBack.contents = back.cgImage
        item.stackBack.contentsScale = back.scale
        item.stackBack.bounds = CGRect(origin: .zero, size: back.size)
        // Up and to the right of the pin, peeking out behind it.
        item.stackBack.position = CGPoint(x: item.node.bounds.midX + 4, y: item.node.bounds.midY - 4)
    }

    /// The pin image; the node takes its size, centred on the note's point.
    private func setImage(_ image: UIImage, on item: Item) {
        item.pin.contents = image.cgImage
        item.pin.contentsScale = image.scale
        let bounds = CGRect(origin: .zero, size: image.size)
        item.pin.bounds = bounds
        item.node.bounds = bounds
        item.pin.position = CGPoint(x: bounds.midX, y: bounds.midY)
    }

    // MARK: - Frames

    func render(_ frame: CanvasFrame) {
        guard !items.isEmpty else { return }
        let dots = frame.metersPerPoint > Self.dotMetersPerPoint
        if needsPlacement(frame, dots: dots) { place(frame, dots: dots) }
        let visible = CGRect(origin: .zero, size: frame.viewSize).insetBy(dx: -Self.stackRadius, dy: -Self.stackRadius)
        for item in items {
            guard item.isLeader else {
                if !item.node.isHidden { item.node.isHidden = true }
                continue
            }
            let p = frame.viewPoint(forMap: item.pixel)
            let onScreen = visible.contains(p)
            if item.node.isHidden == onScreen { item.node.isHidden = !onScreen }
            if onScreen { item.node.position = p }
        }
    }

    private func needsPlacement(_ frame: CanvasFrame, dots: Bool) -> Bool {
        guard let placement else { return true }
        if placement.dots != dots { return true }
        let zoom = max(frame.camera.zoom, 1e-6)
        // Stacks depend only on zoom and rotation (panning moves every pin alike).
        return abs(zoom / max(placement.zoom, 1e-6) - 1) > 0.02 || abs(frame.camera.rotation - placement.rotation) > 0.02
    }

    /// Works out the stacks for the current zoom and rotation.
    private func place(_ frame: CanvasFrame, dots: Bool) {
        placement = (frame.camera.zoom, frame.camera.rotation, dots)
        let points = items.map { frame.viewPoint(forMap: $0.pixel) }
        // The selected note leads its stack; then newest first (the store's order).
        var order = Array(items.indices)
        if let s = items.firstIndex(where: { $0.isSelected }) {
            order.remove(at: s)
            order.insert(s, at: 0)
        }
        let leaders = NoteClustering.leaders(points: points, order: order,
                                             radius: dots ? Self.dotMergeRadius : Self.stackRadius)
        var counts = [Int](repeating: 0, count: items.count)
        for leader in leaders { counts[leader] += 1 }
        for (i, item) in items.enumerated() {
            if item.isDot != dots {
                item.isDot = dots
                applyArt(item)
                item.applyScale()
            }
            item.isLeader = leaders[i] == i
            let count = item.isLeader ? counts[i] : 1
            if count != item.count {
                item.count = count
                if count > 1 {
                    let badge = NoteArt.badge(count)
                    item.badge.contents = badge.cgImage
                    item.badge.contentsScale = badge.scale
                    item.badge.bounds = CGRect(origin: .zero, size: badge.size)
                }
            }
            let stacked = item.isLeader && count > 1 && !dots
            item.badge.isHidden = !stacked
            item.stackBack.isHidden = !stacked
            if stacked {
                // On the pin's top-right edge.
                let b = item.node.bounds
                let r = (item.note.photo != nil ? NoteArt.photoSize : NoteArt.textSize) / 2
                item.badge.position = CGPoint(x: b.midX + r * 0.75, y: b.midY - r * 0.75)
            }
        }
    }

    func hitTest(_ point: CGPoint, frame: CanvasFrame) -> CanvasHit? {
        var best: CanvasHit?
        for item in items where item.isLeader && !item.node.isHidden {
            let p = frame.viewPoint(forMap: item.pixel)
            let d = hypot(p.x - point.x, p.y - point.y)
            // Finger reach (22 pt), or the pin itself when it is larger.
            guard d <= max(22, item.radius + 4), d < (best?.distance ?? .infinity) else { continue }
            best = CanvasHit(item: GeoItemRef(kind: .note, id: item.note.id), poi: nil, distance: d)
        }
        return best
    }
}

/// Greedy stacking of map pins in screen space.
enum NoteClustering {
    /// For each point, the index of the point leading its stack: taking the points in `order`,
    /// each one not yet in a stack leads one with every point not yet in a stack within `radius`
    /// of it. A grid of `radius` cells keeps it near-linear.
    static func leaders(points: [CGPoint], order: [Int], radius: CGFloat) -> [Int] {
        var leader = Array(points.indices)
        guard points.count > 1, radius > 0 else { return leader }
        struct Cell: Hashable {
            var x: Int
            var y: Int
        }
        func cell(_ p: CGPoint) -> Cell {
            Cell(x: Int((p.x / radius).rounded(.down)), y: Int((p.y / radius).rounded(.down)))
        }
        var grid: [Cell: [Int]] = [:]
        for (i, p) in points.enumerated() where p.x.isFinite && p.y.isFinite {
            grid[cell(p), default: []].append(i)
        }
        var taken = [Bool](repeating: false, count: points.count)
        let r2 = radius * radius
        for i in order where i >= 0 && i < points.count && !taken[i] {
            taken[i] = true
            let p = points[i]
            guard p.x.isFinite, p.y.isFinite else { continue }
            let c = cell(p)
            for dx in -1...1 {
                for dy in -1...1 {
                    for j in grid[Cell(x: c.x + dx, y: c.y + dy)] ?? [] where !taken[j] {
                        let q = points[j]
                        let ddx = q.x - p.x, ddy = q.y - p.y
                        if ddx * ddx + ddy * ddy <= r2 {
                            taken[j] = true
                            leader[j] = i
                        }
                    }
                }
            }
        }
        return leader
    }
}

/// The notes' pin bitmaps, rendered once and reused as layer contents. Main thread only.
@MainActor
enum NoteArt {
    /// Photo pins: a round thumbnail with a 2 pt white ring.
    static let photoSize: CGFloat = 34
    /// Text notes: an amber marker, the size of the map's POI markers.
    static let textSize: CGFloat = 26
    /// Zoomed out.
    static let dotSize: CGFloat = 10
    /// Room around a pin for its shadow.
    static let padding: CGFloat = 5

    private static var photoPins: [UUID: UIImage] = [:]
    private static var cache: [String: UIImage] = [:]

    /// A photo note's pin: the thumbnail (or an amber placeholder while it loads), white ring,
    /// soft shadow; centred in the image.
    static func photoPin(_ thumbnail: UIImage?, id: UUID) -> UIImage {
        if thumbnail != nil, let hit = photoPins[id] { return hit }
        if thumbnail == nil, let hit = cache["photo-placeholder"] { return hit }
        let image = disc(size: photoSize) { cg, inner in
            if let thumbnail {
                cg.saveGState()
                UIBezierPath(ovalIn: inner).addClip()
                thumbnail.draw(in: aspectFill(thumbnail.size, in: inner))
                cg.restoreGState()
            } else {
                fill(cg, inner, color: Theme.uiNote)
                drawSymbol("photo", pointSize: 13, in: inner)
            }
        }
        if thumbnail != nil {
            if photoPins.count > 400 { photoPins.removeAll() }
            photoPins[id] = image
        } else {
            cache["photo-placeholder"] = image
        }
        return image
    }

    /// A text note's pin: an amber disc with a note glyph.
    static func textPin() -> UIImage {
        if let hit = cache["text"] { return hit }
        let image = disc(size: textSize) { cg, inner in
            fill(cg, inner, color: Theme.uiNote)
            drawSymbol("note.text", pointSize: 12, in: inner)
        }
        cache["text"] = image
        return image
    }

    /// Zoomed out: a small amber dot with a white ring.
    static func dot() -> UIImage {
        if let hit = cache["dot"] { return hit }
        let d = dotSize
        let pad: CGFloat = 3
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        let image = UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let cg = ctx.cgContext
            let rect = CGRect(x: pad, y: pad, width: d, height: d)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 0.5), blur: 1.5, color: UIColor.black.withAlphaComponent(0.4).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: rect).fill()
            cg.restoreGState()
            Theme.uiNote.setFill()
            UIBezierPath(ovalIn: rect.insetBy(dx: 1.5, dy: 1.5)).fill()
        }
        cache["dot"] = image
        return image
    }

    /// The card peeking out behind a stack's top pin.
    static func stackBack(photo: Bool) -> UIImage {
        let key = photo ? "stack-photo" : "stack-text"
        if let hit = cache[key] { return hit }
        let image = disc(size: photo ? photoSize : textSize) { cg, inner in
            fill(cg, inner, color: OverlayArt.mix(Theme.uiNote, .black, 0.25))
        }
        cache[key] = image
        return image
    }

    /// A stack's count: white digits on deep amber in a white-ringed capsule.
    static func badge(_ count: Int) -> UIImage {
        let text = count > 99 ? "99+" : "\(count)"
        let key = "badge-\(text)"
        if let hit = cache[key] { return hit }
        let font = UIFont.systemFont(ofSize: 11, weight: .bold)
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.white]
        let textSize = (text as NSString).size(withAttributes: attributes)
        let height: CGFloat = 18
        let width = max(height, ceil(textSize.width) + 10)
        let pad: CGFloat = 2
        let size = CGSize(width: width + pad * 2, height: height + pad * 2)
        let image = UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let cg = ctx.cgContext
            let rect = CGRect(x: pad, y: pad, width: width, height: height)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 0.5), blur: 1.5, color: UIColor.black.withAlphaComponent(0.35).cgColor)
            UIColor.white.setFill()
            UIBezierPath(roundedRect: rect, cornerRadius: height / 2).fill()
            cg.restoreGState()
            OverlayArt.mix(Theme.uiNote, .black, 0.2).setFill()
            UIBezierPath(roundedRect: rect.insetBy(dx: 1.5, dy: 1.5), cornerRadius: (height - 3) / 2).fill()
            (text as NSString).draw(at: CGPoint(x: rect.midX - textSize.width / 2, y: rect.midY - textSize.height / 2),
                                    withAttributes: attributes)
        }
        cache[key] = image
        return image
    }

    // MARK: - Drawing

    /// A white-ringed disc of diameter `size` with a soft shadow; `content` fills the inside.
    private static func disc(size d: CGFloat, content: (CGContext, CGRect) -> Void) -> UIImage {
        let pad = padding
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        return UIGraphicsImageRenderer(size: size, format: OverlayArt.format()).image { ctx in
            let cg = ctx.cgContext
            let rect = CGRect(x: pad, y: pad, width: d, height: d)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 1.5), blur: 4, color: UIColor.black.withAlphaComponent(0.4).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: rect).fill()
            cg.restoreGState()
            content(cg, rect.insetBy(dx: 2, dy: 2))
        }
    }

    /// A disc in `color` with a gentle top-to-bottom gradient, like the map's POI markers.
    private static func fill(_ cg: CGContext, _ rect: CGRect, color: UIColor) {
        cg.saveGState()
        UIBezierPath(ovalIn: rect).addClip()
        let top = OverlayArt.mix(color, .white, 0.18).cgColor
        let bottom = OverlayArt.mix(color, .black, 0.08).cgColor
        if let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [top, bottom] as CFArray, locations: [0, 1]) {
            cg.drawLinearGradient(gradient, start: CGPoint(x: rect.midX, y: rect.minY), end: CGPoint(x: rect.midX, y: rect.maxY),
                                  options: [])
        }
        cg.restoreGState()
    }

    private static func drawSymbol(_ name: String, pointSize: CGFloat, in rect: CGRect) {
        let config = UIImage.SymbolConfiguration(pointSize: pointSize, weight: .bold)
        guard let symbol = UIImage(systemName: name, withConfiguration: config)?
            .withTintColor(.white, renderingMode: .alwaysOriginal) else { return }
        let s = symbol.size
        symbol.draw(in: CGRect(x: rect.midX - s.width / 2, y: rect.midY - s.height / 2, width: s.width, height: s.height))
    }

    /// `content` scaled to cover `rect`, centred.
    nonisolated static func aspectFill(_ content: CGSize, in rect: CGRect) -> CGRect {
        guard content.width > 0, content.height > 0 else { return rect }
        let scale = max(rect.width / content.width, rect.height / content.height)
        let w = content.width * scale, h = content.height * scale
        return CGRect(x: rect.midX - w / 2, y: rect.midY - h / 2, width: w, height: h)
    }
}
