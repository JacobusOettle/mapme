import CoreGraphics
import Foundation
import SwiftUI
import UIKit

/// One control point: a pixel on the user's map and the same spot on Earth.
struct CalibrationPair: Identifiable, Equatable {
    let id: UUID
    /// Base-image pixel.
    var pixel: CGPoint
    var coordinate: LatLon
    /// Paired with the GPS fix ("I'm here") rather than a satellite tap.
    var fromGPS: Bool

    init(id: UUID = UUID(), pixel: CGPoint, coordinate: LatLon, fromGPS: Bool) {
        self.id = id
        self.pixel = pixel
        self.coordinate = coordinate
        self.fromGPS = fromGPS
    }
}

/// Result of fitting the control points in the core.
struct CalibrationFit: Equatable {
    var georef: Georeference
    /// Ground metres.
    var rms: Double
    /// Per-point residual in ground metres, same order as the pairs.
    var residuals: [Double]
    /// "similarity" | "affine" | "homography"
    var model: String

    /// True once there are more points than the fitted model needs (similarity 2, affine 3,
    /// homography 4). Before that the residuals are ~0 by construction and say nothing about accuracy.
    var isChecked: Bool {
        let needed = model == "similarity" ? 2 : (model == "homography" ? 4 : 3)
        return residuals.count > needed
    }

    static func confidence(forRMS rms: Double) -> Double {
        rms < 10 ? 0.9 : rms < 30 ? 0.7 : 0.5
    }

    static func residualTint(_ meters: Double) -> Color {
        meters < 15 ? Theme.good : meters < 50 ? Theme.warn : Theme.bad
    }

    /// Fit `pairs` with mapme-core. Returns the fit or a user-facing reason it failed.
    static func make(from pairs: [CalibrationPair]) -> Result<CalibrationFit, CalibrationFitError> {
        guard pairs.count >= 2 else { return .failure(CalibrationFitError("Add at least two points.")) }
        for i in 0..<pairs.count {
            for j in (i + 1)..<pairs.count {
                let a = pairs[i], b = pairs[j]
                if hypot(a.pixel.x - b.pixel.x, a.pixel.y - b.pixel.y) < 4 {
                    return .failure(CalibrationFitError("Points \(i + 1) and \(j + 1) are on the same spot of your map. Delete one of them."))
                }
                if Geodesy.distance(a.coordinate, b.coordinate) < 1 {
                    return .failure(CalibrationFitError("Points \(i + 1) and \(j + 1) are at the same place on Earth. Delete one of them."))
                }
            }
        }
        let request = ControlPointRequest(
            points: pairs.map { ControlPointInput(x: Double($0.pixel.x), y: Double($0.pixel.y),
                                                  lat: $0.coordinate.lat, lon: $0.coordinate.lon) },
            model: "auto"
        )
        let response: ControlPointResponse
        do {
            response = try MapMeCore.fitControlPoints(request)
        } catch {
            return .failure(CalibrationFitError(error.localizedDescription))
        }
        guard response.status == "ok", let m = response.pixelToMercator, m.count == 9, m.allSatisfy({ $0.isFinite }) else {
            return .failure(CalibrationFitError(response.message ?? "These points don\u{2019}t give a usable fit. Spread them further apart."))
        }
        let homography = Homography(m)
        guard homography.inverse != nil else {
            return .failure(CalibrationFitError("These points don\u{2019}t give a usable fit. Spread them further apart."))
        }
        let rms = (response.rmsM ?? 0).isFinite ? (response.rmsM ?? 0) : 0
        let georef = Georeference(pixelToMercator: homography, method: .manual,
                                  confidence: confidence(forRMS: rms), rmsMeters: rms)
        // A degenerate solve can still be invertible by a hair; insist on a sane map scale.
        let n = CGFloat(pairs.count)
        let centroid = CGPoint(x: pairs.reduce(0) { $0 + $1.pixel.x } / n, y: pairs.reduce(0) { $0 + $1.pixel.y } / n)
        let scale = georef.metersPerPixel(atPixel: centroid)
        guard scale.isFinite, scale > 0.001, scale < 10_000 else {
            return .failure(CalibrationFitError("These points don\u{2019}t give a usable fit. Spread them further apart."))
        }
        return .success(CalibrationFit(georef: georef, rms: rms, residuals: response.residualsM ?? [],
                                       model: response.model ?? ""))
    }
}

struct CalibrationFitError: Error, Equatable {
    let message: String
    init(_ message: String) { self.message = message }
}

/// Which pane the user should tap next.
enum CalibrationTarget: Equatable {
    case map, satellite, neither
}

struct CalibrationStep: Equatable {
    let symbol: String
    let text: String
    let target: CalibrationTarget
}

/// Colours of numbered control points, shared by both panes.
enum CalibrationPalette {
    static let uiColors: [UIColor] = [
        .systemYellow, .systemPink, .systemGreen, .systemOrange,
        .systemPurple, .systemTeal, .systemIndigo, .systemMint,
    ]

    static func uiColor(_ index: Int) -> UIColor { uiColors[((index % uiColors.count) + uiColors.count) % uiColors.count] }
    static func color(_ index: Int) -> Color { Color(uiColor: uiColor(index)) }

    static func swatch(_ rgb: [Int]) -> Color {
        guard rgb.count >= 3 else { return .clear }
        return Color(red: Double(rgb[0]) / 255, green: Double(rgb[1]) / 255, blue: Double(rgb[2]) / 255)
    }

    static func uiSwatch(_ rgb: [Int]) -> UIColor {
        guard rgb.count >= 3 else { return .clear }
        return UIColor(red: CGFloat(rgb[0]) / 255, green: CGFloat(rgb[1]) / 255, blue: CGFloat(rgb[2]) / 255, alpha: 1)
    }
}
