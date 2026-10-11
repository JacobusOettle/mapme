#if DEBUG
import CoreGraphics
import CoreImage
import CoreImage.CIFilterBuiltins
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Debug builds only (UI-test hook `-importSamplePhoto` and unit tests): a believable phone photo of
/// a printed map, made from a flat rendering of it. The page is projected through a pinhole camera
/// (so its outline is a real perspective view), laid on a dark table with a soft shadow, lit
/// unevenly with a warm cast, slightly blurred and saved as a JPEG with camera EXIF.
enum SyntheticPhoto {
    struct Camera {
        /// 24 MP, the default photo size of current iPhones.
        var width = 4284
        var height = 5712
        /// 35 mm-equivalent focal length (an iPhone's 1× lens); written to the EXIF too.
        var focalLength35mm = 26.0
        /// Page tilt about the photo's horizontal axis (positive: top edge away from the camera), degrees.
        var tiltX = 12.0
        /// Tilt about the vertical axis, degrees.
        var tiltY = -7.0
        /// Rotation in the image plane, degrees.
        var roll = 2.5
        /// Page outline height as a fraction of the photo height.
        var fill = 0.75
        /// Page centre offset as a fraction of the page size (negative y: up).
        var shiftX = 0.02
        var shiftY = -0.06

        /// Focal length in pixels.
        var focalPixels: Double {
            let w = Double(width)
            let h = Double(height)
            return focalLength35mm / PhotoGeometry.fullFrameDiagonal * (w * w + h * h).squareRoot()
        }
    }

    struct Result {
        /// The photo as rendered (before JPEG compression).
        var image: CGImage
        /// JPEG with camera EXIF.
        var jpeg: Data
        /// The paper's true corners in photo pixels.
        var corners: PhotoQuad
        /// Page pixels (of the `page` raster) → photo pixels.
        var pageToPhoto: Homography
    }

    /// Corners of a page with width ÷ height `pageAspect`, seen through `camera`, in photo pixels.
    static func projectedCorners(pageAspect: Double, camera: Camera) -> PhotoQuad {
        let f = camera.focalPixels
        let cx = Double(camera.width) / 2
        let cy = Double(camera.height) / 2
        let rx = -camera.tiltX * .pi / 180
        let ry = camera.tiltY * .pi / 180
        let rz = camera.roll * .pi / 180
        // Page in its own plane (z = 0), centred, height 1, x right, y down.
        let pw = pageAspect
        let ph = 1.0
        let corners: [SIMD3<Double>] = [SIMD3<Double>(-pw / 2, -ph / 2, 0), SIMD3<Double>(pw / 2, -ph / 2, 0),
                                        SIMD3<Double>(pw / 2, ph / 2, 0), SIMD3<Double>(-pw / 2, ph / 2, 0)]
        func rotate(_ p: SIMD3<Double>) -> SIMD3<Double> {
            // Rx, then Ry, then Rz.
            let a = SIMD3<Double>(p.x, p.y * cos(rx) - p.z * sin(rx), p.y * sin(rx) + p.z * cos(rx))
            let b = SIMD3<Double>(a.x * cos(ry) + a.z * sin(ry), a.y, -a.x * sin(ry) + a.z * cos(ry))
            return SIMD3<Double>(b.x * cos(rz) - b.y * sin(rz), b.x * sin(rz) + b.y * cos(rz), b.z)
        }
        func project(distance: Double) -> [CGPoint] {
            corners.map { c -> CGPoint in
                var p = rotate(c)
                p.x += camera.shiftX * pw
                p.y += camera.shiftY * ph
                p.z += distance
                return CGPoint(x: cx + f * p.x / p.z, y: cy + f * p.y / p.z)
            }
        }
        // Distance at which the outline is `fill` of the photo height.
        var near = 0.05
        var far = 1000.0
        for _ in 0..<80 {
            let mid = (near + far) / 2
            let ys = project(distance: mid).map { Double($0.y) }
            let span = (ys.max() ?? 0) - (ys.min() ?? 0)
            if span > camera.fill * Double(camera.height) {
                near = mid
            } else {
                far = mid
            }
        }
        let points = project(distance: (near + far) / 2)
        return PhotoQuad(points: points) ?? PhotoQuad.full(CGSize(width: camera.width, height: camera.height))
    }

    /// A photo of `page` (a flat, upright rendering of the sheet).
    static func make(page: CGImage, camera: Camera = Camera()) throws -> Result {
        let pageSize = CGSize(width: page.width, height: page.height)
        let quad = projectedCorners(pageAspect: Double(page.width) / Double(page.height), camera: camera)
        let pageCorners = PhotoQuad.full(pageSize).points
        guard let pageToPhoto = PhotoGeometry.homography(from: pageCorners, to: quad.points) else {
            throw PhotoRectifyError.renderFailed
        }
        let canvasWidth = CGFloat(camera.width)
        let canvasHeight = CGFloat(camera.height)
        let canvas = CGRect(x: 0, y: 0, width: canvasWidth, height: canvasHeight)

        // Pre-shrink the page close to its size in the photo (area-averaged, like a sensor would),
        // so the perspective warp does not alias thin contour lines.
        let edges = quad.edgeLengths
        let shrink = min(1, max(edges.left, edges.right) / Double(page.height) * 1.1)
        let source: CGImage
        if shrink < 0.99 {
            let longSide = Int(Double(max(page.width, page.height)) * shrink)
            source = MapPhotoRectifier.downscaled(page, maxLongSide: longSide) ?? page
        } else {
            source = page
        }

        let perspective = CIFilter.perspectiveTransform()
        perspective.inputImage = CIImage(cgImage: source)
        perspective.topLeft = PhotoGeometry.coreImagePoint(fromPixel: quad.topLeft, imageHeight: canvasHeight)
        perspective.topRight = PhotoGeometry.coreImagePoint(fromPixel: quad.topRight, imageHeight: canvasHeight)
        perspective.bottomRight = PhotoGeometry.coreImagePoint(fromPixel: quad.bottomRight, imageHeight: canvasHeight)
        perspective.bottomLeft = PhotoGeometry.coreImagePoint(fromPixel: quad.bottomLeft, imageHeight: canvasHeight)
        guard let paper = perspective.outputImage else { throw PhotoRectifyError.renderFailed }

        // Dark wooden table.
        let table = CIFilter.linearGradient()
        table.point0 = CGPoint(x: 0, y: 0)
        table.point1 = CGPoint(x: canvasWidth, y: canvasHeight)
        table.color0 = CIColor(red: 0.20, green: 0.14, blue: 0.10)
        table.color1 = CIColor(red: 0.11, green: 0.08, blue: 0.06)
        guard let background = table.outputImage?.cropped(to: canvas) else { throw PhotoRectifyError.renderFailed }

        // Soft shadow under the sheet.
        let darken = CIFilter.colorMatrix()
        darken.inputImage = paper
        darken.rVector = CIVector(x: 0, y: 0, z: 0, w: 0)
        darken.gVector = CIVector(x: 0, y: 0, z: 0, w: 0)
        darken.bVector = CIVector(x: 0, y: 0, z: 0, w: 0)
        darken.aVector = CIVector(x: 0, y: 0, z: 0, w: 0.55)
        let blurShadow = CIFilter.gaussianBlur()
        blurShadow.inputImage = darken.outputImage?.transformed(by: CGAffineTransform(translationX: 10, y: -14))
        blurShadow.radius = 22
        guard let shadow = blurShadow.outputImage else { throw PhotoRectifyError.renderFailed }
        let scene = paper.composited(over: shadow.composited(over: background)).cropped(to: canvas)

        // Uneven light: brighter towards the top right, falling off by ~30 %.
        let light = CIFilter.radialGradient()
        light.center = CGPoint(x: canvasWidth * 0.72, y: canvasHeight * 0.80)
        light.radius0 = Float(canvasHeight * 0.08)
        light.radius1 = Float(canvasHeight * 1.05)
        light.color0 = CIColor(red: 1, green: 1, blue: 1)
        light.color1 = CIColor(red: 0.68, green: 0.68, blue: 0.68)
        let lit = CIFilter.multiplyCompositing()
        lit.inputImage = light.outputImage?.cropped(to: canvas)
        lit.backgroundImage = scene
        // Warm indoor white balance.
        let warm = CIFilter.colorMatrix()
        warm.inputImage = lit.outputImage
        warm.rVector = CIVector(x: 1.0, y: 0, z: 0, w: 0)
        warm.gVector = CIVector(x: 0, y: 0.95, z: 0, w: 0)
        warm.bVector = CIVector(x: 0, y: 0, z: 0.84, w: 0)
        warm.aVector = CIVector(x: 0, y: 0, z: 0, w: 1)
        // A touch of lens softness.
        let soften = CIFilter.gaussianBlur()
        soften.inputImage = warm.outputImage?.clampedToExtent()
        soften.radius = 0.8
        guard let finished = soften.outputImage?.cropped(to: canvas),
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let image = context.createCGImage(finished, from: canvas, format: .RGBA8, colorSpace: space)
        else { throw PhotoRectifyError.renderFailed }
        guard let jpeg = jpegWithCameraEXIF(image, focalLength35mm: camera.focalLength35mm) else {
            throw PhotoRectifyError.renderFailed
        }
        return Result(image: image, jpeg: jpeg, corners: quad, pageToPhoto: pageToPhoto)
    }

    /// The bundled sample sheet (Debug builds embed fixtures/grootkloof/grootkloof.pdf as SampleMap.pdf)
    /// photographed with the default camera.
    static func sample() throws -> Result {
        guard let url = Bundle.main.url(forResource: "SampleMap", withExtension: "pdf") else {
            throw ImportError.unreadable("SampleMap.pdf is not in this build")
        }
        let (page, _) = try MapImporter.renderPDF(at: url)
        return try make(page: page)
    }

    /// Corners of the sheet in the bundled real photo (fixtures/grootkloof/photo/truth.json
    /// `page_quad`), as fractions of the photo's width and height, TL, TR, BR, BL.
    static let samplePhotoSheet: [(x: Double, y: Double)] = [
        (0.1175, 0.0873), (0.8445, 0.0873), (0.9049, 0.9150), (0.0248, 0.9510),
    ]

    /// What the `-importSamplePhoto` launch argument imports: the real phone photo of the printed
    /// sheet (on a wooden table, folded in four) when the Debug build bundles it as
    /// `SamplePhoto.jpg`, else a synthetic photo of the bundled sample map (`sample()`). `corners`
    /// is the sheet's outline, which the importer offers only when detection finds nothing.
    static func samplePhoto() throws -> (jpeg: Data, corners: PhotoQuad) {
        if let url = Bundle.main.url(forResource: "SamplePhoto", withExtension: "jpg"),
           let data = try? Data(contentsOf: url),
           let size = uprightSize(of: data) {
            let points = samplePhotoSheet.map { CGPoint(x: CGFloat($0.x) * size.width, y: CGFloat($0.y) * size.height) }
            if let corners = PhotoQuad(points: points) {
                return (data, corners)
            }
        }
        let synthetic = try sample()
        return (synthetic.jpeg, synthetic.corners)
    }

    /// Pixel size of an image as the importer decodes it (EXIF orientation applied).
    private static func uprightSize(of data: Data) -> CGSize? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.doubleValue,
              let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.doubleValue,
              width > 0, height > 0
        else { return nil }
        let orientation = (properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
        // Orientations 5…8 turn the image a quarter turn.
        return orientation >= 5 ? CGSize(width: height, height: width) : CGSize(width: width, height: height)
    }

    private static let context = CIContext(options: [.cacheIntermediates: false])

    private static func jpegWithCameraEXIF(_ image: CGImage, focalLength35mm: Double) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        let exif: [CFString: Any] = [
            kCGImagePropertyExifExposureTime: 1.0 / 120.0,
            kCGImagePropertyExifFNumber: 1.8,
            kCGImagePropertyExifFocalLenIn35mmFilm: Int(focalLength35mm.rounded()),
            kCGImagePropertyExifISOSpeedRatings: [100],
        ]
        let tiff: [CFString: Any] = [
            kCGImagePropertyTIFFMake: "MapMe",
            kCGImagePropertyTIFFModel: "Synthetic camera",
        ]
        let properties: [CFString: Any] = [
            kCGImageDestinationLossyCompressionQuality: 0.9,
            kCGImagePropertyExifDictionary: exif,
            kCGImagePropertyTIFFDictionary: tiff,
        ]
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }
}
#endif
