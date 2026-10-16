import SwiftUI

// The battery saver's views (docs/design/explorer.md, Battery saver). Stubs: the battery saver
// fills them in.

/// Side column: a small leaf badge while the saver is on; tap for what it changes and the drain
/// measured this session. Empty while the saver is off.
@MainActor
struct BatterySaverBadge: View {
    let context: MapScreenContext

    var body: some View {
        EmptyView()
    }
}

/// Settings → Battery: Off / With Low Power Mode / Always, what it changes, the honest estimate.
@MainActor
struct BatterySettingsSection: View {
    var body: some View {
        EmptyView()
    }
}
