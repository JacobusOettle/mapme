import SwiftUI
import UIKit

/// Design tokens. The app is dark-only: a near-black UI, a hot red for "you are
/// here", an electric cyan accent for interactive chrome.
enum Theme {
    static let background = Color(red: 0.04, green: 0.05, blue: 0.06)
    static let surface = Color(red: 0.09, green: 0.10, blue: 0.12)
    static let surfaceRaised = Color(red: 0.13, green: 0.14, blue: 0.17)
    static let stroke = Color.white.opacity(0.10)
    static let textPrimary = Color.white
    static let textSecondary = Color.white.opacity(0.62)
    static let accent = Color(red: 0.20, green: 0.85, blue: 0.95)
    static let userRed = Color(red: 1.0, green: 0.18, blue: 0.22)
    static let track = Color(red: 1.0, green: 0.42, blue: 0.20)
    static let good = Color(red: 0.30, green: 0.90, blue: 0.50)
    static let warn = Color(red: 1.0, green: 0.78, blue: 0.20)
    static let bad = Color(red: 1.0, green: 0.35, blue: 0.35)

    // UIKit equivalents for the map canvas.
    static let uiUserRed = UIColor(red: 1.0, green: 0.18, blue: 0.22, alpha: 1)
    static let uiTrack = UIColor(red: 1.0, green: 0.42, blue: 0.20, alpha: 1)
    static let uiAccent = UIColor(red: 0.20, green: 0.85, blue: 0.95, alpha: 1)
    static let uiBackground = UIColor(red: 0.04, green: 0.05, blue: 0.06, alpha: 1)
    static let uiPOI = UIColor(red: 1.0, green: 0.85, blue: 0.30, alpha: 1)

    static let corner: CGFloat = 16
}

/// Frosted dark capsule used for floating map controls.
struct GlassBackground: ViewModifier {
    var cornerRadius: CGFloat = Theme.corner

    func body(content: Content) -> some View {
        content
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous).stroke(Theme.stroke, lineWidth: 1))
    }
}

extension View {
    func glass(cornerRadius: CGFloat = Theme.corner) -> some View {
        modifier(GlassBackground(cornerRadius: cornerRadius))
    }
}
