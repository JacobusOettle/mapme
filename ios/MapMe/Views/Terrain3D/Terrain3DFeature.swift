import Foundation

/// The 3D view (docs/design/explorer.md, 3D view): the feature's lifecycle hooks, called by
/// `FeatureHooks`. The view needs no service of its own: it reads the map, the terrain
/// (`AppModel.terrain`), the geo items, the track and your position when it opens.
@MainActor
enum Terrain3DFeature {
    /// `-open3D` was given and the 3D view has not been opened for it yet.
    private static var openAtLaunch = false

    static func attach(_ model: AppModel) {}

    /// Debug launch argument: `-open3D` (opens the 3D view once the sample map shows).
    static func appDidLaunch(_ model: AppModel) async {
        #if DEBUG
        openAtLaunch = ProcessInfo.processInfo.arguments.contains("-open3D")
        #endif
    }

    /// True once after `-open3D`: the 3D button of the first located map shown opens the view.
    static func takeOpenAtLaunch() -> Bool {
        guard openAtLaunch else { return false }
        openAtLaunch = false
        return true
    }
}
