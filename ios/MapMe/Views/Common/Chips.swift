import SwiftUI

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

/// Colours and names for point-of-interest kinds.
enum POIKindStyle {
    static func color(for kind: POIKind) -> Color {
        switch kind {
        case .peak, .spotHeight: return Color(red: 0.95, green: 0.62, blue: 0.32)
        case .water: return Color(red: 0.35, green: 0.68, blue: 1.0)
        case .place: return Theme.accent
        case .parking: return Color(red: 0.55, green: 0.65, blue: 1.0)
        case .shelter: return Theme.good
        case .viewpoint: return Color(red: 0.78, green: 0.55, blue: 1.0)
        case .waypoint: return Theme.userRed
        case .other: return Color.white.opacity(0.7)
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
