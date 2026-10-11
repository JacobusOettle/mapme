import SwiftUI
import UIKit

/// Georeference status capsule: how the map was located and how accurately
/// ("Contours ±18 m", "Grid ±2 m", "Manual ±6 m", "Not located").
struct StatusChip: View {
    let georef: Georeference?
    /// Icon only plus accuracy, for tight spaces.
    var compact: Bool = false

    var body: some View {
        let tint = Self.tint(for: georef)
        HStack(spacing: 5) {
            Image(systemName: Self.symbol(for: georef))
                .font(.caption2.weight(.bold))
            Text(compact ? Self.accuracyText(for: georef) ?? Self.methodText(for: georef) : Self.text(for: georef))
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .lineLimit(1)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(.ultraThinMaterial, in: Capsule())
        .background(tint.opacity(0.12), in: Capsule())
        .overlay(Capsule().stroke(tint.opacity(0.35), lineWidth: 1))
        .accessibilityElement(children: .combine)
    }

    static func methodText(for georef: Georeference?) -> String {
        guard let g = georef else { return "Not located" }
        switch g.method {
        case .grid: return "Grid"
        case .labels: return "Labels"
        case .contours: return "Contours"
        case .manual: return "Manual"
        }
    }

    static func accuracyText(for georef: Georeference?) -> String? {
        guard let rms = georef?.rmsMeters, rms.isFinite else { return nil }
        return "±\(max(1, Int(rms.rounded()))) m"
    }

    static func text(for georef: Georeference?) -> String {
        let method = methodText(for: georef)
        guard let accuracy = accuracyText(for: georef) else { return method }
        return "\(method) \(accuracy)"
    }

    static func symbol(for georef: Georeference?) -> String {
        guard let g = georef else { return "questionmark.circle.fill" }
        switch g.method {
        case .grid, .labels: return "number.circle.fill"
        case .contours: return "mountain.2.circle.fill"
        case .manual: return "hand.tap.fill"
        }
    }

    static func tint(for georef: Georeference?) -> Color {
        guard let g = georef else { return Theme.warn }
        if g.confidence < 0.5 { return Theme.warn }
        return g.method == .manual ? Theme.accent : Theme.good
    }
}

/// An SF Symbol in a tinted circle (POI kinds, settings rows, list icons).
struct SymbolBadge: View {
    let symbol: String
    var color: Color = Theme.accent
    var size: CGFloat = 32

    var body: some View {
        ZStack {
            Circle()
                .fill(color.opacity(0.18))
            Circle()
                .strokeBorder(color.opacity(0.45), lineWidth: 1)
            Image(systemName: symbol)
                .font(.system(size: size * 0.42, weight: .semibold))
                .foregroundStyle(color)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// Small tinted capsule with an optional icon ("3 · 12 m", "±8 m").
struct TintedChip: View {
    let text: String
    var symbol: String? = nil
    var tint: Color = Theme.accent

    var body: some View {
        HStack(spacing: 4) {
            if let symbol {
                Image(systemName: symbol)
                    .font(.caption2.weight(.bold))
            }
            Text(text)
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .lineLimit(1)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(tint.opacity(0.14), in: Capsule())
        .overlay(Capsule().stroke(tint.opacity(0.35), lineWidth: 1))
    }
}

/// Colours, glyphs and names for point-of-interest kinds (Apple Maps style category colours).
enum POIKindStyle {
    static func color(for kind: POIKind) -> Color {
        Color(uiColor: uiColor(for: kind))
    }

    static func uiColor(for kind: POIKind) -> UIColor {
        switch kind {
        case .peak: return UIColor(red: 0.85, green: 0.45, blue: 0.18, alpha: 1)
        case .spotHeight: return UIColor(red: 0.64, green: 0.50, blue: 0.36, alpha: 1)
        case .water: return UIColor(red: 0.16, green: 0.62, blue: 0.96, alpha: 1)
        case .place: return UIColor(red: 0.62, green: 0.36, blue: 0.90, alpha: 1)
        case .parking: return UIColor(red: 0.25, green: 0.42, blue: 0.98, alpha: 1)
        case .shelter: return UIColor(red: 0.18, green: 0.70, blue: 0.38, alpha: 1)
        case .viewpoint: return UIColor(red: 0.07, green: 0.68, blue: 0.66, alpha: 1)
        case .waypoint: return UIColor(red: 1.0, green: 0.23, blue: 0.19, alpha: 1)
        case .other: return UIColor(red: 0.50, green: 0.52, blue: 0.56, alpha: 1)
        }
    }

    /// White SF Symbol drawn inside the round marker.
    static func glyph(for kind: POIKind) -> String {
        switch kind {
        case .peak: return "mountain.2.fill"
        case .spotHeight: return "triangle.fill"
        case .water: return "drop.fill"
        case .place: return "mappin"
        case .parking: return "parkingsign"
        case .shelter: return "house.fill"
        case .viewpoint: return "binoculars.fill"
        case .waypoint: return "flag.fill"
        case .other: return "mappin"
        }
    }

    static func name(for kind: POIKind) -> String {
        switch kind {
        case .place: return "Place"
        case .peak: return "Peak"
        case .spotHeight: return "Spot height"
        case .water: return "Water"
        case .parking: return "Parking"
        case .shelter: return "Shelter"
        case .viewpoint: return "Viewpoint"
        case .waypoint: return "Waypoint"
        case .other: return "Point"
        }
    }

    static func sourceName(for source: POISource) -> String {
        switch source {
        case .mapLabel: return "on map"
        case .openStreetMap: return "OpenStreetMap"
        case .user: return "your waypoint"
        }
    }
}

/// Apple Maps style round marker: a category-coloured disc with a white glyph and a white ring.
struct POIGlyphMarker: View {
    let kind: POIKind
    var size: CGFloat = 34

    var body: some View {
        ZStack {
            Circle()
                .fill(Color.white)
            Circle()
                .fill(POIKindStyle.color(for: kind).gradient)
                .padding(size * 0.07)
            Image(systemName: POIKindStyle.glyph(for: kind))
                .font(.system(size: size * 0.4, weight: .semibold))
                .foregroundStyle(Color.white)
        }
        .frame(width: size, height: size)
        .shadow(color: Color.black.opacity(0.25), radius: 2, y: 1)
        .accessibilityHidden(true)
    }
}
