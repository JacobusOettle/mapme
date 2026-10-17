import Foundation
import UIKit

// The 3D view's look per map appearance (docs/design/explorer.md, 3D view: Look). Dark stays dark
// but readable: the terrain beyond the sheet on a slate-blue ground, light enough for its light and
// shade to show once lit, a low key light for slope contrast, and a twilight sky whose glow outlines
// the block. Paper is daylight: a soft blue sky, a pale horizon, the paper map in sunlight.

/// Sky, haze, light, ground, sides and labels of the 3D view for one appearance.
struct Terrain3DLook: Equatable, Sendable {
    /// An sRGB colour, components 0…1.
    struct RGB: Equatable, Sendable {
        var r: Double
        var g: Double
        var b: Double

        func uiColor(alpha: Double = 1) -> UIColor {
            UIColor(red: CGFloat(r), green: CGFloat(g), blue: CGFloat(b), alpha: CGFloat(alpha))
        }

        /// RGBA 0…255, opaque: the core's render takes its background so.
        var bytes: [Int] {
            [r, g, b].map { (c: Double) -> Int in Int((min(max(c, 0), 1) * 255).rounded()) } + [255]
        }
    }

    /// A colour of the sky's gradient on the spherical backdrop: `location` 0 is the zenith, 0.5
    /// the horizon, 1 straight down (past the last stop the colour stays).
    struct SkyStop: Equatable, Sendable {
        var location: Double
        var color: RGB
    }

    let appearance: MapAppearance
    /// Top to bottom. Below the horizon it shows around the block's far edge.
    let sky: [SkyStop]
    /// Haze over the far terrain: the horizon's colour, so the land fades into it.
    let fog: RGB
    /// Beyond the sheet: the paper the terrain's contours and hillshade are drawn on.
    let ground: RGB
    /// The core's hillshade strength there (0…1).
    let hillshade: Double
    /// Light from everywhere and the key light, as multiples of the texture's colour (SceneKit's
    /// intensity 1000 is 1×). The key light comes from the north-west like the hillshade's sun.
    let ambient: Double
    let key: Double
    let keyElevationDegrees: Double
    /// White screened over the dark sheet (0…1): its near-black paper a little lighter, so the
    /// key light's shading shows on it too (on near-black it barely does).
    let sheetLift: Double
    /// The block's sides: at the top, and at the bottom.
    let sideTop: RGB
    let sideBottom: RGB
    /// Billboard labels: the text, and the halo around it.
    let labelInk: RGB
    let labelHalo: RGB
    let labelHaloAlpha: Double

    static func of(_ appearance: MapAppearance) -> Terrain3DLook {
        switch appearance {
        case .dark: return Self.dark
        case .paper: return Self.paper
        }
    }

    /// Night, readable: slate ground (the 2D map's near-black would hide the relief), a deep blue
    /// zenith over a blue-grey horizon glow, dark again below it so the block stands out.
    static let dark = Terrain3DLook(
        appearance: .dark,
        sky: [
            SkyStop(location: 0, color: RGB(r: 0.04, g: 0.06, b: 0.11)),
            SkyStop(location: 0.35, color: RGB(r: 0.10, g: 0.14, b: 0.22)),
            SkyStop(location: 0.5, color: RGB(r: 0.30, g: 0.36, b: 0.46)),
            SkyStop(location: 0.56, color: RGB(r: 0.14, g: 0.16, b: 0.21)),
            SkyStop(location: 0.75, color: RGB(r: 0.06, g: 0.07, b: 0.09)),
        ],
        fog: RGB(r: 0.30, g: 0.36, b: 0.46),
        ground: RGB(r: 0.165, g: 0.188, b: 0.235),
        hillshade: 0.55,
        ambient: 0.38,
        key: 1.1,
        keyElevationDegrees: 35,
        sheetLift: 0.12,
        sideTop: RGB(r: 0.15, g: 0.17, b: 0.21),
        sideBottom: RGB(r: 0.05, g: 0.06, b: 0.08),
        labelInk: RGB(r: 1, g: 1, b: 1),
        labelHalo: RGB(r: 0.05, g: 0.05, b: 0.06),
        labelHaloAlpha: 0.88
    )

    /// Daylight: soft blue to a pale horizon, blue-grey haze below it; the paper map lit a little
    /// below 1× so sunny slopes do not burn out; dark ink on a white halo, as the 2D paper map.
    static let paper = Terrain3DLook(
        appearance: .paper,
        sky: [
            SkyStop(location: 0, color: RGB(r: 0.36, g: 0.56, b: 0.82)),
            SkyStop(location: 0.3, color: RGB(r: 0.55, g: 0.71, b: 0.88)),
            SkyStop(location: 0.5, color: RGB(r: 0.82, g: 0.88, b: 0.94)),
            SkyStop(location: 0.53, color: RGB(r: 0.72, g: 0.77, b: 0.82)),
            SkyStop(location: 0.75, color: RGB(r: 0.56, g: 0.61, b: 0.67)),
        ],
        fog: RGB(r: 0.82, g: 0.88, b: 0.94),
        ground: RGB(r: 0.91, g: 0.90, b: 0.87),
        hillshade: 0.45,
        ambient: 0.4,
        key: 0.8,
        keyElevationDegrees: 35,
        sheetLift: 0,
        sideTop: RGB(r: 0.62, g: 0.58, b: 0.52),
        sideBottom: RGB(r: 0.30, g: 0.27, b: 0.23),
        labelInk: RGB(r: 0.12, g: 0.12, b: 0.13),
        labelHalo: RGB(r: 1, g: 1, b: 1),
        labelHaloAlpha: 0.95
    )

    /// Light on level ground: about 1×, the texture as drawn.
    var levelLight: Double {
        ambient + key * sin(keyElevationDegrees * .pi / 180)
    }

    /// Which way the key light travels in scene coordinates (x east, y up, z south): from the
    /// north-west, `elevationDegrees` above the horizon, towards the south-east (a unit vector).
    static func keyDirection(elevationDegrees: Double) -> SIMD3<Float> {
        let e: Double = elevationDegrees * .pi / 180
        let across: Double = cos(e) / 2.0.squareRoot()
        return SIMD3<Float>(Float(across), Float(-sin(e)), Float(across))
    }
}
