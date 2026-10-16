import Foundation

/// Map auto-switching, the world view and the terrain beyond a sheet (docs/design/explorer.md,
/// feature "maps"): the maps feature's lifecycle hooks, called by `FeatureHooks`.
@MainActor
enum MapsFeature {
    static func attach(_ model: AppModel) {
        model.mapSwitcher.attach(model)
    }

    /// Debug launch arguments: `-openWorldView`, `-seedSecondSampleMap` (and `-openSampleOverview`:
    /// open that second map once it exists, for the auto-switching UI test).
    static func appDidLaunch(_ model: AppModel) async {
        #if DEBUG
        let args = ProcessInfo.processInfo.arguments
        let openOverview = args.contains("-openSampleOverview")
        if args.contains("-seedSecondSampleMap") || openOverview {
            MapsDebugSeeds.seedSecondSampleMap(model, open: openOverview)
        }
        if args.contains("-openWorldView") {
            MapsDebugSeeds.openWorldViewWhenReady(model)
        }
        #endif
    }

    static func mapDidLocate(_ map: MapDocument, model: AppModel) {
        model.mapSwitcher.mapDidLocate(map.id)
    }

    static func mapDidDelete(_ id: UUID, model: AppModel) {
        model.mapSwitcher.forget(id)
    }
}
