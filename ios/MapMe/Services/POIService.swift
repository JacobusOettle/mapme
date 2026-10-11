// OWNER: pipeline agent. Replace the implementation; keep this public API.
import Foundation

/// Points of interest from OpenStreetMap (Overpass API) inside a map's footprint, cached per map.
final class POIService {
    func fetchOSM(for map: MapDocument) async -> [MapPOI] { [] }
}
