// OWNER: pipeline agent. Replace the implementation; keep this public API.
import CoreGraphics
import CoreLocation
import Foundation

enum GeorefStage: Equatable {
    case readingText
    case analysing
    case downloadingTerrain(done: Int, total: Int)
    case matchingContours
    case finished

    var label: String {
        switch self {
        case .readingText: return "Reading map labels…"
        case .analysing: return "Looking for grid lines…"
        case .downloadingTerrain(let d, let t): return "Downloading terrain \(d)/\(t)…"
        case .matchingContours: return "Matching contour lines to terrain…"
        case .finished: return "Done"
        }
    }
}

struct GeorefOutcome {
    var georef: Georeference?
    /// User-facing summary ("Located from grid labels (±2 m)" / reason it failed).
    var message: String
    /// Labels read off the map (place names, peaks), already converted to coordinates.
    var pois: [MapPOI]
    var response: GeorefResponse?
}

/// OCR → mapme-core session → DEM download loop → result.
final class GeoreferenceService {
    init(dem: DEMService = .shared) {}

    /// `progress(stage, fraction 0…1)` may be called from any thread.
    func locate(image: CGImage, dpi: Double?, gps: CLLocation?, contourRGB: [Int]?, searchRadiusKm: Double,
                progress: @escaping (GeorefStage, Double) -> Void) async -> GeorefOutcome {
        GeorefOutcome(georef: nil, message: "Not implemented", pois: [], response: nil)
    }

    /// Cancel the running `locate`.
    func cancel() {}
}
