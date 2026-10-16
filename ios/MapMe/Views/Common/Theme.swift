import SwiftUI
import UIKit

/// Design tokens. The app is dark-only: a near-black UI, a hot red for "you are
/// here", an electric cyan accent for interactive chrome. The live map follows
/// Apple Maps: paper on Apple dark grey, a vivid pink dotted trail, glass chrome.
enum Theme {
    static let background = Color(red: 0.04, green: 0.05, blue: 0.06)
    static let surface = Color(red: 0.09, green: 0.10, blue: 0.12)
    static let surfaceRaised = Color(red: 0.13, green: 0.14, blue: 0.17)
    static let stroke = Color.white.opacity(0.10)
    static let textPrimary = Color.white
    static let textSecondary = Color.white.opacity(0.62)
    static let accent = Color(red: 0.20, green: 0.85, blue: 0.95)
    static let userRed = Color(red: 1.0, green: 0.18, blue: 0.22)
    /// The walked trail (system pink).
    static let track = Color(red: 1.0, green: 0.22, blue: 0.37)
    static let good = Color(red: 0.30, green: 0.90, blue: 0.50)
    static let warn = Color(red: 1.0, green: 0.78, blue: 0.20)
    static let bad = Color(red: 1.0, green: 0.35, blue: 0.35)
    /// Apple's dark grey behind the paper map (#1C1C1E).
    static let mapBackground = Color(red: 0.110, green: 0.110, blue: 0.118)
    /// The record button's red.
    static let record = Color(red: 1.0, green: 0.23, blue: 0.19)

    // UIKit equivalents for the map canvas.
    static let uiUserRed = UIColor(red: 1.0, green: 0.18, blue: 0.22, alpha: 1)
    static let uiTrack = UIColor(red: 1.0, green: 0.22, blue: 0.37, alpha: 1)
    static let uiTrailStart = UIColor(red: 0.20, green: 0.80, blue: 0.36, alpha: 1)
    static let uiAccent = UIColor(red: 0.20, green: 0.85, blue: 0.95, alpha: 1)
    static let uiBackground = UIColor(red: 0.04, green: 0.05, blue: 0.06, alpha: 1)
    static let uiMapBackground = UIColor(red: 0.110, green: 0.110, blue: 0.118, alpha: 1)
    /// Paper shown under tiles that are still loading.
    static let uiPaper = UIColor(red: 0.96, green: 0.95, blue: 0.92, alpha: 1)
    /// Paper of the dark map (#212124): the core's dark tiles turn paper into exactly this colour
    /// (docs/ARCHITECTURE.md, "Dark map"), so keep the two in step.
    static let uiNightPaper = UIColor(red: 0.13, green: 0.13, blue: 0.14, alpha: 1)
    /// Around the dark sheet: near-black, so the sheet's edge stays visible.
    static let uiDarkMapSurround = UIColor(red: 0.035, green: 0.035, blue: 0.04, alpha: 1)
    static let uiPOI = UIColor(red: 1.0, green: 0.85, blue: 0.30, alpha: 1)

    // Explorer features (docs/design/explorer.md, Look): routes, notes, the terrain beyond a sheet.

    /// Planned routes: electric violet, apart from the pink trail, the red puck and the map's inks.
    static let route = Color(red: 0.49, green: 0.36, blue: 1.0)
    static let uiRoute = UIColor(red: 0.49, green: 0.36, blue: 1.0, alpha: 1)
    /// Imported routes and tracks (GPX from Gaia, AllTrails, friends).
    static let importedRoute = Color(red: 0.0, green: 0.78, blue: 0.75)
    static let uiImportedRoute = UIColor(red: 0.0, green: 0.78, blue: 0.75, alpha: 1)
    /// Notes and photos on the map.
    static let note = Color(red: 1.0, green: 0.62, blue: 0.04)
    static let uiNote = UIColor(red: 1.0, green: 0.62, blue: 0.04, alpha: 1)
    /// Paper of the terrain drawn beyond a sheet's edge: a shade off the sheet's own, so the
    /// real map stays recognisable (dark map, then paper appearance).
    static let uiOffMapNightPaper = UIColor(red: 0.105, green: 0.105, blue: 0.115, alpha: 1)
    static let uiOffMapPaper = UIColor(red: 0.91, green: 0.90, blue: 0.87, alpha: 1)
    /// Hairline along a sheet's edge where the terrain beyond it shows.
    static let uiSheetEdge = UIColor(white: 1, alpha: 0.35)
    static let uiSheetEdgePaper = UIColor(white: 0, alpha: 0.3)

    static let corner: CGFloat = 16
    /// The smallest comfortable hit target, points (Apple's Human Interface Guidelines).
    static let minTapTarget: CGFloat = 44
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

    /// Floating chrome over the map: Liquid Glass on iOS 26, a frosted material with a hairline
    /// and a soft shadow on iOS 17 and 18.
    func floatingChrome<S: InsettableShape>(_ shape: S, interactive: Bool = false) -> some View {
        modifier(FloatingChrome(shape: shape, interactive: interactive))
    }
}

/// See `View.floatingChrome(_:interactive:)`.
struct FloatingChrome<S: InsettableShape>: ViewModifier {
    let shape: S
    var interactive = false

    @ViewBuilder
    func body(content: Content) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            content.glassEffect(interactive ? Glass.regular.interactive() : Glass.regular, in: shape)
        } else {
            materialBody(content)
        }
        #else
        materialBody(content)
        #endif
    }

    private func materialBody(_ content: Content) -> some View {
        content
            .background(.regularMaterial, in: shape)
            .overlay(shape.strokeBorder(Color.white.opacity(0.12), lineWidth: 0.5))
            .shadow(color: Color.black.opacity(0.28), radius: 10, y: 4)
    }
}
