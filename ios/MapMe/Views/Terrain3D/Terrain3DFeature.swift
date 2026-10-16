import Foundation

/// The 3D view (docs/design/explorer.md, 3D view): the feature's lifecycle hooks, called by
/// `FeatureHooks`.
@MainActor
enum Terrain3DFeature {
    static func attach(_ model: AppModel) {}

    /// Debug launch argument: `-open3D` (opens the 3D view once the sample map shows).
    static func appDidLaunch(_ model: AppModel) async {}
}
