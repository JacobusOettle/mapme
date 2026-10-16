import SwiftUI

/// The map screen's side column, under the layers and tracking controls: one round glass button
/// per feature (Apple Maps style, 44 pt, 10 pt apart), each declared in its feature's files and
/// empty until the feature fills it in. Top to bottom: the maps here, 3D, routes, notes, then the
/// battery saver's indicator.
@MainActor
struct MapSideColumn: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        VStack(spacing: 10) {
            MapsHereButton(context: context, map: map)          // maps: the maps covering you, world view
            Terrain3DButton(context: context, map: map)         // 3d: the requested 3D button
            RouteToolsButton(context: context, map: map)        // routes: plan, routes, import GPX
            AddNoteButton(context: context, map: map)           // notes: note, photo
            BatterySaverBadge(context: context)                 // battery: shown while the saver is on
        }
    }
}

/// Notices under the map's title: the features' banners, then the screen's toast.
@MainActor
struct MapTopBanners: View {
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            WalkedOffMapBanner(context: context, map: map)      // maps: "You've walked off …"
            RouteGuidanceBanner(context: context, map: map)     // routes: off route, progress, daylight
            if let toast = context.toast {
                MapToastView(toast: toast) { context.dismissToast(toast.id) }
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .frame(maxWidth: 360, alignment: .leading)
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: context.toast?.id)
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
