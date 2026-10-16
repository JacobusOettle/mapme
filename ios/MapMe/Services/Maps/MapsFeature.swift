import Foundation

/// Map auto-switching, the world view and the terrain beyond a sheet (docs/design/explorer.md,
/// feature "maps"): the maps feature's lifecycle hooks, called by `FeatureHooks`.
@MainActor
enum MapsFeature {
    static func attach(_ model: AppModel) {
        model.mapSwitcher.attach(model)
    }

    /// Debug launch arguments: `-openWorldView`, `-seedSecondSampleMap`.
    static func appDidLaunch(_ model: AppModel) async {}

    static func mapDidLocate(_ map: MapDocument, model: AppModel) {}

    static func mapDidDelete(_ id: UUID, model: AppModel) {}
}
