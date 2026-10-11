import UIKit

/// Small pre-rendered bitmaps for the map overlay (heading beam, POI markers, balloon pin, halo
/// labels, text pills, scale bar, numbered pins), rendered once and reused as CALayer contents.
/// Main thread only.
enum OverlayArt {
    /// Diameter of a POI marker disc (points).
    static let markerSize: CGFloat = 26
    /// Bitmap padding around the marker for its shadow.
    static let markerPadding: CGFloat = 5
    static let badgeSize: CGFloat = markerSize
    static let pinDiameter: CGFloat = 28
    static let pinPointer: CGFloat = 8
    /// Balloon pin for the selected POI: head diameter and total height (tip at the bottom).
    static let balloonHead: CGFloat = 40
    static let balloonHeight: CGFloat = 54

    private static var markerCache: [String: UIImage] = [:]
    private static var balloonCache: [String: UIImage] = [:]
    private static var beamCache: [String: UIImage] = [:]
    private static var haloTextCache: [String: UIImage] = [:]

    static func format() -> UIGraphicsImageRendererFormat {
        let f = UIGraphicsImageRendererFormat.preferred()
        f.opaque = false
        return f
    }

    // MARK: - Colours

    /// `a` blended toward `b` by `amount` (0…1), in sRGB.
    static func mix(_ a: UIColor, _ b: UIColor, _ amount: CGFloat) -> UIColor {
        var r1: CGFloat = 0, g1: CGFloat = 0, b1: CGFloat = 0, a1: CGFloat = 0
        var r2: CGFloat = 0, g2: CGFloat = 0, b2: CGFloat = 0, a2: CGFloat = 0
        a.getRed(&r1, green: &g1, blue: &b1, alpha: &a1)
        b.getRed(&r2, green: &g2, blue: &b2, alpha: &a2)
        let t = min(max(amount, 0), 1)
        return UIColor(red: r1 + (r2 - r1) * t, green: g1 + (g2 - g1) * t,
                       blue: b1 + (b2 - b1) * t, alpha: a1 + (a2 - a1) * t)
    }

    private static func cacheKey(_ color: UIColor) -> String {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        color.getRed(&r, green: &g, blue: &b, alpha: &a)
        return String(format: "%.3f,%.3f,%.3f,%.3f", r, g, b, a)
    }

    // MARK: - Heading beam

    /// Apple Maps style "flashlight": a wedge pointing up from the centre of a `radius`-sized
    /// square, `widthDegrees` wide, fading out radially from `color` at `peakAlpha` and softly at
    /// its sides. Rendered per pixel once per colour and width, then cached.
    static func beam(color: UIColor, widthDegrees: Int, radius: CGFloat, peakAlpha: CGFloat = 0.55) -> UIImage {
        let key = "\(cacheKey(color))|\(widthDegrees)|\(radius)|\(peakAlpha)"
        if let hit = beamCache[key] { return hit }
        let scale = format().scale
        let n = max(2, Int((radius * 2 * scale).rounded()))
        var red: CGFloat = 0, green: CGFloat = 0, blue: CGFloat = 0, alpha: CGFloat = 0
        color.getRed(&red, green: &green, blue: &blue, alpha: &alpha)
        let half = Double(widthDegrees) * .pi / 360
        let inner = half * 0.45
        let r = Double(radius)
        // The puck covers the first ~11 pt, so the fade starts at its edge.
        let fadeStart = 10.0
        let s = Double(scale)
        var pixels = [UInt8](repeating: 0, count: n * n * 4)
        for y in 0..<n {
            let dy = (Double(y) + 0.5) / s - r
            for x in 0..<n {
                let dx = (Double(x) + 0.5) / s - r
                let dist = (dx * dx + dy * dy).squareRoot()
                guard dist < r else { continue }
                let theta = abs(atan2(dx, -dy))
                guard theta < half else { continue }
                let angular = smoothstep(from: half, to: inner, theta)
                let radial = pow(min(1, max(0, (r - dist) / (r - fadeStart))), 1.25)
                let a = Double(peakAlpha) * angular * radial
                guard a > 0.002 else { continue }
                let i = (y * n + x) * 4
                pixels[i] = byte(Double(red) * a)
                pixels[i + 1] = byte(Double(green) * a)
                pixels[i + 2] = byte(Double(blue) * a)
                pixels[i + 3] = byte(a)
            }
        }
        let image: UIImage
        if let provider = CGDataProvider(data: Data(pixels) as CFData),
           let cg = CGImage(width: n, height: n, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: n * 4,
                            space: CGColorSpaceCreateDeviceRGB(),
                            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                            provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent) {
            image = UIImage(cgImage: cg, scale: scale, orientation: .up)
        } else {
            image = UIImage()
        }
        beamCache[key] = image
        return image
    }

    /// A 0…1 channel value as a byte.
    private static func byte(_ unit: Double) -> UInt8 {
        let scaled: Double = (unit * 255).rounded()
        return UInt8(min(255.0, max(0.0, scaled)))
    }

    /// 1 at `to`, 0 at `from`, smooth in between (either order).
    private static func smoothstep(from e0: Double, to e1: Double, _ x: Double) -> Double {
        let t = min(max((x - e0) / (e1 - e0), 0), 1)
        return t * t * (3 - 2 * t)
    }

    // MARK: - POI markers

    /// Apple Maps style marker: a category-coloured disc with a white glyph, a white ring and a
    /// soft shadow. The disc is centred in the image (padding on every side for the shadow).
    static func marker(for kind: POIKind) -> UIImage {
        let key = kind.rawValue
        if let hit = markerCache[key] { return hit }
        let d = markerSize
        let pad = markerPadding
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        let color = POIKindStyle.uiColor(for: kind)
        let image = UIGraphicsImageRenderer(size: size, format: format()).image { ctx in
            let cg = ctx.cgContext
            let disc = CGRect(x: pad, y: pad, width: d, height: d)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.35).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: disc).fill()
            cg.restoreGState()
            let inner = disc.insetBy(dx: 2, dy: 2)
            fillVerticalGradient(cg, clip: CGPath(ellipseIn: inner, transform: nil), rect: inner,
                                 top: mix(color, .white, 0.18), bottom: mix(color, .black, 0.08))
            drawSymbol(POIKindStyle.glyph(for: kind), pointSize: 12, in: inner)
        }
        markerCache[key] = image
        return image
    }

    /// The selected POI: a larger Apple balloon pin (teardrop) in the category colour with a white
    /// glyph. Its tip is at the bottom centre of the image.
    static func balloon(for kind: POIKind) -> UIImage {
        let key = kind.rawValue
        if let hit = balloonCache[key] { return hit }
        let head = balloonHead
        let pad: CGFloat = 6
        let size = CGSize(width: head + pad * 2, height: balloonHeight + pad)
        let color = POIKindStyle.uiColor(for: kind)
        let image = UIGraphicsImageRenderer(size: size, format: format()).image { ctx in
            let cg = ctx.cgContext
            let headRect = CGRect(x: pad, y: pad, width: head, height: head)
            let tip = CGPoint(x: size.width / 2, y: size.height - 1)
            let outline = balloonPath(head: headRect, tip: tip)
            cg.saveGState()
            cg.setShadow(offset: CGSize(width: 0, height: 2), blur: 5, color: UIColor.black.withAlphaComponent(0.4).cgColor)
            UIColor.white.setFill()
            outline.fill()
            cg.restoreGState()
            let innerHead = headRect.insetBy(dx: 2.5, dy: 2.5)
            let inner = balloonPath(head: innerHead, tip: CGPoint(x: tip.x, y: tip.y - 4))
            let body = CGRect(x: innerHead.minX, y: innerHead.minY, width: innerHead.width, height: tip.y - innerHead.minY)
            fillVerticalGradient(cg, clip: inner.cgPath, rect: body,
                                 top: mix(color, .white, 0.18), bottom: mix(color, .black, 0.1))
            drawSymbol(POIKindStyle.glyph(for: kind), pointSize: 17, in: innerHead)
        }
        balloonCache[key] = image
        return image
    }

    /// Circle head joined to a point below it by two tangent-ish curves.
    private static func balloonPath(head: CGRect, tip: CGPoint) -> UIBezierPath {
        let c = CGPoint(x: head.midX, y: head.midY)
        let r = head.width / 2
        let p = UIBezierPath()
        // Start at the lower right of the circle, go round over the top to the lower left.
        let a0 = CGFloat.pi * 0.25
        let a1 = CGFloat.pi * 0.75
        p.addArc(withCenter: c, radius: r, startAngle: a0, endAngle: a1, clockwise: false)
        let left = CGPoint(x: c.x + r * cos(a1), y: c.y + r * sin(a1))
        let right = CGPoint(x: c.x + r * cos(a0), y: c.y + r * sin(a0))
        p.addQuadCurve(to: tip, controlPoint: CGPoint(x: left.x + r * 0.28, y: left.y + r * 0.42))
        p.addQuadCurve(to: right, controlPoint: CGPoint(x: right.x - r * 0.28, y: right.y + r * 0.42))
        p.close()
        return p
    }

    /// Fills `clip` with a top-to-bottom gradient spanning `rect`.
    private static func fillVerticalGradient(_ cg: CGContext, clip: CGPath, rect: CGRect, top: UIColor, bottom: UIColor) {
        cg.saveGState()
        cg.addPath(clip)
        cg.clip()
        if let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
                                     colors: [top.cgColor, bottom.cgColor] as CFArray, locations: [0, 1]) {
            cg.drawLinearGradient(gradient, start: CGPoint(x: rect.midX, y: rect.minY),
                                  end: CGPoint(x: rect.midX, y: rect.maxY), options: [])
        }
        cg.restoreGState()
    }

    private static func drawSymbol(_ name: String, pointSize: CGFloat, in rect: CGRect) {
        let config = UIImage.SymbolConfiguration(pointSize: pointSize, weight: .semibold)
        guard let symbol = UIImage(systemName: name, withConfiguration: config)?
            .withTintColor(.white, renderingMode: .alwaysOriginal) else { return }
        let s = symbol.size
        symbol.draw(in: CGRect(x: rect.midX - s.width / 2, y: rect.midY - s.height / 2, width: s.width, height: s.height))
    }

    // MARK: - Text

    /// Room around halo text for the halo.
    private static let haloPad: CGFloat = 3

    /// Size of the image `haloText` makes (cheap: measures, does not draw).
    static func haloTextSize(_ text: String, fontSize: CGFloat = 12, weight: UIFont.Weight = .semibold) -> CGSize {
        let font = UIFont.systemFont(ofSize: fontSize, weight: weight)
        let s = (text as NSString).size(withAttributes: [.font: font])
        return CGSize(width: ceil(s.width) + haloPad * 2, height: ceil(s.height) + haloPad * 2)
    }

    /// Map label: text with a soft halo (no pill), like the labels on Apple Maps.
    static func haloText(_ text: String, color: UIColor, halo: UIColor, fontSize: CGFloat = 12,
                         weight: UIFont.Weight = .semibold) -> UIImage {
        let key = "\(text)|\(cacheKey(color))|\(cacheKey(halo))|\(fontSize)|\(weight.rawValue)"
        if let hit = haloTextCache[key] { return hit }
        let font = UIFont.systemFont(ofSize: fontSize, weight: weight)
        let fill: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
        let ring: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: halo]
        let pad = haloPad
        let size = haloTextSize(text, fontSize: fontSize, weight: weight)
        let image = UIGraphicsImageRenderer(size: size, format: format()).image { _ in
            let origin = CGPoint(x: pad, y: pad)
            // A round halo: the text stamped around a small circle, then the text on top.
            for (radius, steps) in [(CGFloat(1.7), 16), (CGFloat(0.85), 8)] {
                for i in 0..<steps {
                    let a = CGFloat(i) / CGFloat(steps) * 2 * .pi
                    (text as NSString).draw(at: CGPoint(x: origin.x + cos(a) * radius, y: origin.y + sin(a) * radius),
                                            withAttributes: ring)
                }
            }
            (text as NSString).draw(at: origin, withAttributes: fill)
        }
        // Labels change with units and selection only; keep the cache bounded anyway.
        if haloTextCache.count > 600 { haloTextCache.removeAll() }
        haloTextCache[key] = image
        return image
    }

    /// Rounded dark pill with text.
    static func pill(_ text: String, fontSize: CGFloat = 12, weight: UIFont.Weight = .semibold,
                     textColor: UIColor = UIColor(white: 0.95, alpha: 1),
                     background: UIColor = UIColor(red: 0.05, green: 0.06, blue: 0.08, alpha: 0.78)) -> UIImage {
        let font = UIFont.systemFont(ofSize: fontSize, weight: weight)
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: textColor]
        let textSize = (text as NSString).size(withAttributes: attributes)
        let size = CGSize(width: ceil(textSize.width) + 16, height: ceil(textSize.height) + 8)
        let canvas = CGSize(width: size.width + 8, height: size.height + 8)
        return UIGraphicsImageRenderer(size: canvas, format: format()).image { ctx in
            let rect = CGRect(origin: CGPoint(x: 4, y: 3), size: size)
            let shape = UIBezierPath(roundedRect: rect, cornerRadius: size.height / 2)
            ctx.cgContext.saveGState()
            ctx.cgContext.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.3).cgColor)
            background.setFill()
            shape.fill()
            ctx.cgContext.restoreGState()
            UIColor(white: 1, alpha: 0.14).setStroke()
            let outline = UIBezierPath(roundedRect: rect.insetBy(dx: 0.25, dy: 0.25), cornerRadius: size.height / 2 - 0.25)
            outline.lineWidth = 0.5
            outline.stroke()
            (text as NSString).draw(at: CGPoint(x: rect.minX + 8, y: rect.minY + 4), withAttributes: attributes)
        }
    }

    // MARK: - Scale bar

    /// Apple Maps style scale: "0 · half · full" labels with halos above a two-segment bar.
    /// The bar starts at x = `scaleInset` in the image.
    static let scaleInset: CGFloat = 6

    static func scaleBar(_ spec: ScaleBarSpec, night: Bool) -> UIImage {
        let ink = night ? UIColor(white: 0.96, alpha: 1) : UIColor(white: 0.08, alpha: 1)
        let halo = night ? UIColor(white: 0, alpha: 0.85) : UIColor(white: 1, alpha: 0.95)
        let font = UIFont.systemFont(ofSize: 11, weight: .semibold)
        let labels: [(String, CGFloat)] = [("0", 0), (spec.halfLabel, spec.length / 2), (spec.label, spec.length)]
        let lastWidth = ceil((spec.label as NSString).size(withAttributes: [.font: font]).width)
        let inset = scaleInset
        let size = CGSize(width: inset + spec.length + lastWidth / 2 + inset + 2, height: 30)
        return UIGraphicsImageRenderer(size: size, format: format()).image { ctx in
            let cg = ctx.cgContext
            let barY: CGFloat = 20
            let barH: CGFloat = 4
            for (text, x) in labels where !text.isEmpty {
                let w = (text as NSString).size(withAttributes: [.font: font]).width
                let origin = CGPoint(x: inset + x - w / 2, y: 2)
                for i in 0..<12 {
                    let a = CGFloat(i) / 12 * 2 * .pi
                    (text as NSString).draw(at: CGPoint(x: origin.x + cos(a) * 1.4, y: origin.y + sin(a) * 1.4),
                                            withAttributes: [.font: font, .foregroundColor: halo])
                }
                (text as NSString).draw(at: origin, withAttributes: [.font: font, .foregroundColor: ink])
            }
            let bar = CGRect(x: inset, y: barY, width: spec.length, height: barH)
            cg.setFillColor(halo.cgColor)
            cg.addPath(UIBezierPath(roundedRect: bar.insetBy(dx: -1.5, dy: -1.5), cornerRadius: 3).cgPath)
            cg.fillPath()
            cg.setFillColor(ink.cgColor)
            cg.fill(CGRect(x: bar.minX, y: bar.minY, width: bar.width / 2, height: bar.height))
            cg.setStrokeColor(ink.cgColor)
            cg.setLineWidth(1)
            cg.stroke(bar.insetBy(dx: 0.5, dy: 0.5))
        }
    }

    // MARK: - Other

    /// Green "start of the trail" dot with a white ring and shadow, centred in the image.
    static func trailStart() -> UIImage {
        let d: CGFloat = 15
        let pad: CGFloat = 4
        let size = CGSize(width: d + pad * 2, height: d + pad * 2)
        return UIGraphicsImageRenderer(size: size, format: format()).image { ctx in
            let rect = CGRect(x: pad, y: pad, width: d, height: d)
            ctx.cgContext.saveGState()
            ctx.cgContext.setShadow(offset: CGSize(width: 0, height: 1), blur: 3, color: UIColor.black.withAlphaComponent(0.35).cgColor)
            UIColor.white.setFill()
            UIBezierPath(ovalIn: rect).fill()
            ctx.cgContext.restoreGState()
            Theme.uiTrailStart.setFill()
            UIBezierPath(ovalIn: rect.insetBy(dx: 3, dy: 3)).fill()
        }
    }

    /// Numbered calibration pin; its tip is at the bottom centre.
    static func pin(label: String, color: UIColor) -> UIImage {
        let d = pinDiameter
        let size = CGSize(width: d + 4, height: d + pinPointer + 4)
        return UIGraphicsImageRenderer(size: size, format: format()).image { _ in
            let circleRect = CGRect(x: 2, y: 2, width: d, height: d)
            let pointer = UIBezierPath()
            pointer.move(to: CGPoint(x: size.width / 2 - 6, y: circleRect.maxY - 4))
            pointer.addLine(to: CGPoint(x: size.width / 2, y: size.height - 1))
            pointer.addLine(to: CGPoint(x: size.width / 2 + 6, y: circleRect.maxY - 4))
            pointer.close()
            UIColor.white.setFill()
            pointer.fill()
            let ring = UIBezierPath(ovalIn: circleRect)
            UIColor.white.setFill()
            ring.fill()
            color.setFill()
            UIBezierPath(ovalIn: circleRect.insetBy(dx: 2.5, dy: 2.5)).fill()
            let font = UIFont.systemFont(ofSize: 13, weight: .heavy)
            let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.black]
            let s = (label as NSString).size(withAttributes: attributes)
            (label as NSString).draw(at: CGPoint(x: circleRect.midX - s.width / 2, y: circleRect.midY - s.height / 2),
                                     withAttributes: attributes)
        }
    }

    /// Upward-pointing arrow head (for the off-screen indicator), centred on the origin.
    static func arrowPath(length: CGFloat = 22, width: CGFloat = 20) -> CGPath {
        let p = UIBezierPath()
        p.move(to: CGPoint(x: 0, y: -length / 2))
        p.addLine(to: CGPoint(x: width / 2, y: length / 2))
        p.addLine(to: CGPoint(x: 0, y: length / 2 - 6))
        p.addLine(to: CGPoint(x: -width / 2, y: length / 2))
        p.close()
        return p.cgPath
    }

    /// Wedge pointing up from the centre of a `radius`-sized square, `width` radians wide.
    static func wedgePath(radius: CGFloat, width: CGFloat) -> CGPath {
        let c = CGPoint(x: radius, y: radius)
        let p = UIBezierPath()
        p.move(to: c)
        p.addArc(withCenter: c, radius: radius, startAngle: -.pi / 2 - width / 2, endAngle: -.pi / 2 + width / 2, clockwise: true)
        p.close()
        return p.cgPath
    }
}
