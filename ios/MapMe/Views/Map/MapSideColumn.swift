import SwiftUI

/// The map screen's side column, under the layers and tracking controls: one round glass button
/// per feature (Apple Maps style, 44 pt, 10 pt apart), each declared in its feature's files and
/// empty until the feature fills it in. Top to bottom: the maps here, 3D, routes, notes, then the
/// battery saver's indicator. When they do not fit above the bottom panel (`maxHeight`, from
/// `MapChromeLayout`: an iPhone SE with a card open, landscape) the rest continue in a second
/// column to the left, never under the panel.
@MainActor
struct MapSideColumn: View {
    let context: MapScreenContext
    let map: MapDocument
    /// Height the column may take under the layers/tracking controls (and the compass).
    var maxHeight: CGFloat = .infinity

    var body: some View {
        let layout = SideColumnLayout(maxHeight: maxHeight, spacing: MapChromeLayout.spacing)
        layout {
            MapsHereButton(context: context, map: map)          // maps: the maps covering you, world view
            Terrain3DButton(context: context, map: map)         // 3d: the requested 3D button
            RouteToolsButton(context: context, map: map)        // routes: plan, routes, import GPX
            AddNoteButton(context: context, map: map)           // notes: note, photo
            BatterySaverBadge(context: context)                 // battery: shown while the saver is on
        }
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: maxHeight)
    }
}

/// Lays the side column's buttons out top to bottom and starts another column to the left when
/// the next one would pass `maxHeight`, so no button ends up under the bottom panel or off the
/// screen. A button a feature leaves empty (the battery saver's badge while the saver is off)
/// takes no room.
struct SideColumnLayout: Layout {
    var maxHeight: CGFloat
    var spacing: CGFloat = 10

    /// The subviews' indices per column, the rightmost column first (sizes of zero are skipped).
    static func columns(_ sizes: [CGSize], maxHeight: CGFloat, spacing: CGFloat) -> [[Int]] {
        var result: [[Int]] = []
        var current: [Int] = []
        var height: CGFloat = 0
        for (i, size) in sizes.enumerated() where size.width > 0 || size.height > 0 {
            let next = current.isEmpty ? size.height : height + spacing + size.height
            if !current.isEmpty, next > maxHeight {
                result.append(current)
                current = [i]
                height = size.height
            } else {
                current.append(i)
                height = next
            }
        }
        if !current.isEmpty { result.append(current) }
        return result
    }

    /// The size of `columns` laid out side by side.
    static func size(of columns: [[Int]], sizes: [CGSize], spacing: CGFloat) -> CGSize {
        var width: CGFloat = 0
        var height: CGFloat = 0
        for (n, column) in columns.enumerated() {
            width += (column.map { sizes[$0].width }.max() ?? 0) + (n > 0 ? spacing : 0)
            let h = column.reduce(CGFloat(0)) { $0 + sizes[$1].height } + spacing * CGFloat(max(column.count - 1, 0))
            height = max(height, h)
        }
        return CGSize(width: width, height: height)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        let columns = Self.columns(sizes, maxHeight: maxHeight, spacing: spacing)
        return Self.size(of: columns, sizes: sizes, spacing: spacing)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        var placed = Set<Int>()
        var x = bounds.maxX
        for column in Self.columns(sizes, maxHeight: maxHeight, spacing: spacing) {
            let width = column.map { sizes[$0].width }.max() ?? 0
            var y = bounds.minY
            for i in column {
                subviews[i].place(at: CGPoint(x: x - width / 2, y: y), anchor: .top, proposal: ProposedViewSize(sizes[i]))
                y += sizes[i].height + spacing
                placed.insert(i)
            }
            x -= width + spacing
        }
        for i in subviews.indices where !placed.contains(i) {
            subviews[i].place(at: CGPoint(x: bounds.maxX, y: bounds.minY), anchor: .topTrailing, proposal: .zero)
        }
    }
}

/// How much room the map screen's floating controls have (docs/design/explorer.md, Map screen):
/// the right-hand column (layers/tracking, compass, the side column) and the banners under the
/// title stop above the bottom panel (selection card, stats panel or a tool's bar) wherever the
/// panel lies under them, so nothing overlaps on a small screen. The panel is at most 520 pt
/// wide and centred: in portrait it spans the whole width; in landscape and on iPad the right
/// column clears it and may run down to the bottom edge.
///
/// iPhone SE (375 × 667 pt, status bar 20): the column starts at 24 pt; below the 88.5 pt
/// layers/tracking capsule the side column has 667 − 8 − 150 − 12 − 24 − 98.5 ≈ 374 pt over the
/// collapsed stats panel (all five buttons, 260 pt, fit) and ≈ 214 pt with a dropped pin's card
/// (four fit; the fifth moves to a second column, as it does when the compass shows). iPhone Pro
/// Max (440 × 956, safe area 62 / 34): ≈ 610 / ≈ 450 pt, one column always. SE in landscape
/// (667 × 375, no status bar): the panel (73…593 pt) leaves the column (611…655 pt) free down to
/// the bottom, 375 − 8 − 4 − 98.5 ≈ 264 pt: one column (two with the compass).
struct MapChromeLayout {
    static let spacing: CGFloat = 10
    static let buttonSide: CGFloat = 44
    /// The layers/tracking capsule: two 44 pt buttons and a hairline.
    static let controlStackHeight: CGFloat = 88.5
    /// The title capsule and the gap under it.
    static let titleHeight: CGFloat = 44 + 8
    /// Kept clear above the bottom panel.
    static let bottomClearance: CGFloat = 12
    /// The bottom panel's widest (cards and the stats panel).
    static let bottomPanelMaxWidth: CGFloat = 520

    /// The whole screen, safe areas included (points).
    var screen: CGSize
    var safe: EdgeInsets
    /// From the screen's bottom edge to the bottom panel.
    var bottomGap: CGFloat
    /// The bottom panel's height (collapsed; the expanded panel floats over the controls).
    var bottomPanelHeight: CGFloat
    var compassShown: Bool

    /// Top of the floating controls.
    var top: CGFloat { safe.top + 4 }

    /// Top of the bottom panel, less the clearance.
    var panelTop: CGFloat { screen.height - bottomGap - bottomPanelHeight - Self.bottomClearance }

    /// The bottom panel's horizontal extent (centred between the side insets, at most 520 pt).
    var panelSpan: ClosedRange<CGFloat> {
        let available = max(0, screen.width - safe.leading - safe.trailing - 16)
        let width = min(Self.bottomPanelMaxWidth, available)
        let mid = safe.leading + 8 + available / 2
        return (mid - width / 2)...(mid + width / 2)
    }

    /// The right column's horizontal extent (one button wide, 12 pt from the edge).
    var rightColumnSpan: ClosedRange<CGFloat> {
        let right = max(Self.buttonSide, screen.width - safe.trailing - 12)
        return (right - Self.buttonSide)...right
    }

    /// Height for the side column under the layers/tracking capsule (and the compass).
    var sideColumnMaxHeight: CGFloat {
        let bottom = panelSpan.overlaps(rightColumnSpan) ? panelTop : screen.height - bottomGap
        let used = Self.controlStackHeight + Self.spacing + (compassShown ? Self.buttonSide + Self.spacing : 0)
        return max(Self.buttonSide, bottom - top - used)
    }

    /// Height for the banners under the title (the panel always lies under them).
    var bannersMaxHeight: CGFloat {
        max(Self.buttonSide, panelTop - top - Self.titleHeight)
    }
}

/// Notices under the map's title: the features' banners, then the screen's toast. Taller than
/// `maxHeight` (landscape, a card open), they scroll instead of running under the bottom panel.
@MainActor
struct MapTopBanners: View {
    let context: MapScreenContext
    let map: MapDocument
    var maxHeight: CGFloat = .infinity

    var body: some View {
        ViewThatFits(in: .vertical) {
            banners
            ScrollView(.vertical) {
                banners
            }
            .scrollIndicators(.hidden)
            .scrollBounceBehavior(.basedOnSize)
        }
        .frame(maxWidth: 360, maxHeight: maxHeight, alignment: .topLeading)
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: context.toast?.id)
    }

    private var banners: some View {
        VStack(alignment: .leading, spacing: 8) {
            WalkedOffMapBanner(context: context, map: map)      // maps: "You've walked off …"
            RouteGuidanceBanner(context: context, map: map)     // routes: off route, progress, daylight
            if let toast = context.toast {
                MapToastView(toast: toast) { context.dismissToast(toast.id) }
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
    }
}

/// A glass capsule notice with an optional action; dismisses itself.
@MainActor
struct MapToastView: View {
    let toast: MapToast
    var onDismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: toast.symbol)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Theme.accent)
            Text(toast.text)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.primary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            if let title = toast.actionTitle, let action = toast.action {
                Button(title) {
                    action()
                    onDismiss()
                }
                .font(.subheadline.weight(.bold))
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("mapToastAction")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .floatingChrome(Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("mapToast")
        .task(id: toast.id) {
            try? await Task.sleep(nanoseconds: UInt64(max(1, toast.duration) * 1_000_000_000))
            if !Task.isCancelled { onDismiss() }
        }
    }
}
