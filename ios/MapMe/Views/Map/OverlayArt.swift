import UIKit

/// Small pre-rendered bitmaps for the map overlay (POI badges, text pills, numbered pins),
/// rendered once and reused as CALayer contents. Main thread only.
enum OverlayArt {
    static let badgeSize: CGFloat = 26
    static let pinDiameter: CGFloat = 28
    static let pinPointer: CGFloat = 8

    private static var badgeCache: [String: UIImage] = [:]

    static func format() -> UIGraphicsImageRendererFormat {
        let f = UIGraphicsImageRendererFormat.preferred()
        f.opaque = false
        return f
    }

    /// Dark round badge with the kind's SF Symbol.
    static func badge(for kind: POIKind) -> UIImage {
        let tint = kind == .waypoint ? Theme.uiAccent : Theme.uiPOI
        let key = kind.rawValue
        if let hit = badgeCache[key] { return hit }
        let size = CGSize(width: badgeSize, height: badgeSize)
        let image = UIGraphicsImageRenderer(size: size, format: format()).image { _ in
            let rect = CGRect(origin: .zero, size: size).insetBy(dx: 1, dy: 1)
            let circle = UIBezierPath(ovalIn: rect)
            UIColor(red: 0.07, green: 0.08, blue: 0.10, alpha: 0.92).setFill()
            circle.fill()
            tint.withAlphaComponent(0.85).setStroke()
            circle.lineWidth = 1.5
            circle.stroke()
            let config = UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            if let symbol = UIImage(systemName: kind.symbolName, withConfiguration: config)?
                .withTintColor(tint, renderingMode: .alwaysOriginal) {
                let s = symbol.size
                symbol.draw(in: CGRect(x: (size.width - s.width) / 2, y: (size.height - s.height) / 2,
                                       width: s.width, height: s.height))
            }
        }
        badgeCache[key] = image
        return image
    }

    /// Rounded dark pill with text.
    static func pill(_ text: String, fontSize: CGFloat = 12, weight: UIFont.Weight = .semibold,
                     textColor: UIColor = UIColor(white: 0.95, alpha: 1),
                     background: UIColor = UIColor(red: 0.05, green: 0.06, blue: 0.08, alpha: 0.78)) -> UIImage {
        let font = UIFont.systemFont(ofSize: fontSize, weight: weight)
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: textColor]
        let textSize = (text as NSString).size(withAttributes: attributes)
        let size = CGSize(width: ceil(textSize.width) + 14, height: ceil(textSize.height) + 6)
        return UIGraphicsImageRenderer(size: size, format: format()).image { _ in
            let rect = CGRect(origin: .zero, size: size)
            background.setFill()
            UIBezierPath(roundedRect: rect, cornerRadius: size.height / 2).fill()
            UIColor(white: 1, alpha: 0.12).setStroke()
            let outline = UIBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), cornerRadius: size.height / 2 - 0.5)
            outline.lineWidth = 1
            outline.stroke()
            (text as NSString).draw(at: CGPoint(x: 7, y: 3), withAttributes: attributes)
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
