import Foundation

/// The app's lifecycle as the explorer features see it (docs/design/explorer.md). Each feature
/// keeps its hooks in its own files (`RoutesFeature`, `MapsFeature`, `NotesFeature`,
/// `BatteryFeature`, `OfflineFeature`, `Terrain3DFeature`); this file only calls them, in a fixed
/// order, so features are added without touching shared code.
@MainActor
enum FeatureHooks {
    /// End of `AppModel.init`, every service made: wire services to each other. No I/O beyond
    /// small files, no network, no permission prompts (the app hosts the unit tests).
    static func attach(_ model: AppModel) {
        BatteryFeature.attach(model)
        OfflineFeature.attach(model)
        RoutesFeature.attach(model)
        MapsFeature.attach(model)
        NotesFeature.attach(model)
        Terrain3DFeature.attach(model)
    }

    /// Once, when the UI appears (location already started). Debug launch arguments for UI tests
    /// are handled here, each by its feature.
    static func appDidLaunch(_ model: AppModel) async {
        await BatteryFeature.appDidLaunch(model)
        await OfflineFeature.appDidLaunch(model)
        await RoutesFeature.appDidLaunch(model)
        await MapsFeature.appDidLaunch(model)
        await NotesFeature.appDidLaunch(model)
        await Terrain3DFeature.appDidLaunch(model)
    }

    /// A map was located or calibrated and saved (`map.georef` is set).
    static func mapDidLocate(_ map: MapDocument, model: AppModel) {
        OfflineFeature.mapDidLocate(map, model: model)
        MapsFeature.mapDidLocate(map, model: model)
    }

    /// A map was deleted from the library (its folder is gone).
    static func mapDidDelete(_ id: UUID, model: AppModel) {
        OfflineFeature.mapDidDelete(id, model: model)
        MapsFeature.mapDidDelete(id, model: model)
    }
}
