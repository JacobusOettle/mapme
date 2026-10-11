// OWNER: pipeline agent. Replace the implementation; keep this public API.
import CoreGraphics
import Foundation

enum OCRService {
    /// Recognise text anywhere on the map, including vertical (rotated) grid labels.
    /// Returned boxes are in base-image pixels, top-left origin.
    static func recognize(in image: CGImage, progress: ((Double) -> Void)? = nil) async throws -> [OCRObservation] { [] }
}
