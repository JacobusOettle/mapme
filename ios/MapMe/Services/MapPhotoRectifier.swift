import CoreGraphics
import CoreImage
import Foundation
import ImageIO

// Photos of paper maps: page corners, the flattening ("rectification") math and the Core Image warp.
//
// Coordinate systems used here (see docs/ARCHITECTURE.md for the app-wide conventions):
// - image pixels: origin top-left, y down (UIKit, CGImage rows, MapDocument);
// - Vision: normalised 0…1, origin bottom-left, y up;
// - Core Image: pixels, origin bottom-left, y up.

/// Four page corners in image pixels (top-left origin, y down), ordered TL, TR, BR, BL: the corners
/// that become the flattened image's top-left, top-right, bottom-right and bottom-left.
struct PhotoQuad: Equatable {
    var topLeft: CGPoint
    var topRight: CGPoint
    var bottomRight: CGPoint
    var bottomLeft: CGPoint

    init(topLeft: CGPoint, topRight: CGPoint, bottomRight: CGPoint, bottomLeft: CGPoint) {
        self.topLeft = topLeft
        self.topRight = topRight
        self.bottomRight = bottomRight
        self.bottomLeft = bottomLeft
    }

    /// Exactly four points, already in TL, TR, BR, BL order.
    init?(points: [CGPoint]) {
        guard points.count == 4 else { return nil }
        self.init(topLeft: points[0], topRight: points[1], bottomRight: points[2], bottomLeft: points[3])
    }

    /// From the stored `[[x, y]]` form (`PhotoSource.quad`).
    init?(json: [[Double]]) {
        guard json.count == 4, json.allSatisfy({ $0.count == 2 && $0[0].isFinite && $0[1].isFinite }) else { return nil }
        self.init(points: json.map { CGPoint(x: $0[0], y: $0[1]) })
    }

    var points: [CGPoint] { [topLeft, topRight, bottomRight, bottomLeft] }
    var json: [[Double]] { points.map { [Double($0.x), Double($0.y)] } }

    /// The whole image.
    static func full(_ size: CGSize) -> PhotoQuad {
        PhotoQuad(topLeft: .zero,
                  topRight: CGPoint(x: size.width, y: 0),
                  bottomRight: CGPoint(x: size.width, y: size.height),
                  bottomLeft: CGPoint(x: 0, y: size.height))
    }

    func mapped(_ transform: (CGPoint) -> CGPoint) -> PhotoQuad {
        PhotoQuad(topLeft: transform(topLeft), topRight: transform(topRight),
                  bottomRight: transform(bottomRight), bottomLeft: transform(bottomLeft))
    }

    /// Pixels of one image size → pixels of the same image at another size.
    func scaled(x sx: CGFloat, y sy: CGFloat) -> PhotoQuad {
        mapped { CGPoint(x: $0.x * sx, y: $0.y * sy) }
    }

    /// Each corner kept inside `size`.
    func clamped(to size: CGSize) -> PhotoQuad {
        mapped { CGPoint(x: min(max($0.x, 0), size.width), y: min(max($0.y, 0), size.height)) }
    }

    /// The same page with the flattened output turned a quarter turn clockwise: the old left edge
    /// becomes the top edge (old BL → new TL, TL → TR, TR → BR, BR → BL).
    var rotatedClockwise: PhotoQuad {
        PhotoQuad(topLeft: bottomLeft, topRight: topLeft, bottomRight: topRight, bottomLeft: bottomRight)
    }

    /// Shoelace area in pixels²; positive when TL → TR → BR → BL runs clockwise on screen (y down).
    var signedArea: Double {
        let p = points
        var sum = 0.0
        for i in 0..<4 {
            let a = p[i]
            let b = p[(i + 1) % 4]
            sum += Double(a.x * b.y - b.x * a.y)
        }
        return sum / 2
    }

    var area: Double { abs(signedArea) }

    /// Convex, non-degenerate and ordered clockwise on screen (TL → TR → BR → BL).
    var isConvexClockwise: Bool {
        let p = points
        for i in 0..<4 {
            let a = p[i]
            let b = p[(i + 1) % 4]
            let c = p[(i + 2) % 4]
            let cross = Double((b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x))
            if !(cross > 0) { return false }
        }
        return true
    }

    /// Interior angles in degrees at TL, TR, BR, BL.
    var interiorAngles: [Double] {
        let p = points
        return (0..<4).map { i -> Double in
            let prev = p[(i + 3) % 4]
            let here = p[i]
            let next = p[(i + 1) % 4]
            let ux = Double(prev.x - here.x), uy = Double(prev.y - here.y)
            let vx = Double(next.x - here.x), vy = Double(next.y - here.y)
            let nu = (ux * ux + uy * uy).squareRoot()
            let nv = (vx * vx + vy * vy).squareRoot()
            guard nu > 0, nv > 0 else { return 0 }
            let c = min(max((ux * vx + uy * vy) / (nu * nv), -1), 1)
            return acos(c) * 180 / .pi
        }
    }

    /// Lengths of the top, right, bottom and left edges in pixels.
    var edgeLengths: (top: Double, right: Double, bottom: Double, left: Double) {
        (PhotoGeometry.distance(topLeft, topRight), PhotoGeometry.distance(topRight, bottomRight),
         PhotoGeometry.distance(bottomRight, bottomLeft), PhotoGeometry.distance(bottomLeft, topLeft))
    }

    var centroid: CGPoint {
        CGPoint(x: (topLeft.x + topRight.x + bottomRight.x + bottomLeft.x) / 4,
                y: (topLeft.y + topRight.y + bottomRight.y + bottomLeft.y) / 4)
    }

    /// The four points ordered TL, TR, BR, BL: clockwise on screen, starting with the point nearest
    /// the image's top-left (smallest x + y). Nil unless there are four points spanning an area.
    static func ordered(_ points: [CGPoint]) -> PhotoQuad? {
        guard points.count == 4, points.allSatisfy({ $0.x.isFinite && $0.y.isFinite }) else { return nil }
        let cx = points.reduce(0.0) { $0 + Double($1.x) } / 4
        let cy = points.reduce(0.0) { $0 + Double($1.y) } / 4
        // With y down, increasing atan2(dy, dx) runs clockwise on screen.
        func angle(_ p: CGPoint) -> Double { atan2(Double(p.y) - cy, Double(p.x) - cx) }
        let sorted = points.sorted { angle($0) < angle($1) }
        var start = 0
        for i in 1..<4 where sorted[i].x + sorted[i].y < sorted[start].x + sorted[start].y {
            start = i
        }
        guard let quad = PhotoQuad(points: (0..<4).map { sorted[(start + $0) % 4] }), quad.area > 1e-6 else { return nil }
        return quad
    }
}

/// What the camera metadata says about an image.
struct PhotoCaptureInfo: Equatable {
    /// Taken by a camera (exposure data present), as opposed to a screenshot, scan or export.
    var isCamera: Bool = false
    /// 35 mm-equivalent focal length (EXIF FocalLenIn35mmFilm).
    var focalLength35mm: Double?

    static func read(from url: URL) -> PhotoCaptureInfo {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return PhotoCaptureInfo() }
        return read(from: source)
    }

    static func read(from data: Data) -> PhotoCaptureInfo {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return PhotoCaptureInfo() }
        return read(from: source)
    }

    private static func read(from source: CGImageSource) -> PhotoCaptureInfo {
        guard CGImageSourceGetCount(source) > 0,
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return PhotoCaptureInfo() }
        return from(properties: properties)
    }

    /// From an image's ImageIO properties dictionary.
    static func from(properties: [CFString: Any]) -> PhotoCaptureInfo {
        guard let exif = properties[kCGImagePropertyExifDictionary] as? [CFString: Any] else { return PhotoCaptureInfo() }
        let exposure = exif[kCGImagePropertyExifExposureTime] != nil || exif[kCGImagePropertyExifFNumber] != nil
        var focal: Double?
        if let f = (exif[kCGImagePropertyExifFocalLenIn35mmFilm] as? NSNumber)?.doubleValue, f > 1, f < 2000 {
            focal = f
        }
        return PhotoCaptureInfo(isCamera: exposure, focalLength35mm: focal)
    }
}

/// Pure geometry for photos of paper maps (no image processing).
enum PhotoGeometry {
    // MARK: Coordinate conversions

    /// Vision's normalised point (origin bottom-left, y up) → image pixel (origin top-left, y down).
    static func pixel(fromVision p: CGPoint, imageSize: CGSize) -> CGPoint {
        CGPoint(x: p.x * imageSize.width, y: (1 - p.y) * imageSize.height)
    }

    /// Image pixel (origin top-left) → Core Image coordinates (origin bottom-left, y up) of an image `imageHeight` px tall.
    static func coreImagePoint(fromPixel p: CGPoint, imageHeight: CGFloat) -> CGPoint {
        CGPoint(x: p.x, y: imageHeight - p.y)
    }

    /// Core Image coordinates (origin bottom-left) → image pixel (origin top-left).
    static func pixel(fromCoreImage p: CGPoint, imageHeight: CGFloat) -> CGPoint {
        CGPoint(x: p.x, y: imageHeight - p.y)
    }

    static func distance(_ a: CGPoint, _ b: CGPoint) -> Double {
        let dx = Double(a.x - b.x)
        let dy = Double(a.y - b.y)
        return (dx * dx + dy * dy).squareRoot()
    }

    // MARK: Does the photo need flattening?

    /// A detected page must cover at least this fraction of the image.
    static let minimumAreaFraction = 0.10
    /// A page covering this much of the image with its corners near the image corners is used as is.
    static let asIsCoverage = 0.92
    /// "Near the image corners": within this fraction of the image diagonal.
    static let asIsCornerDistance = 0.04

    enum Triage: Equatable {
        /// Use the image as it is (no page found, or it already fills the frame: screenshots, scans).
        case asIs
        /// A page in perspective: let the user confirm these corners, then flatten.
        case needsCorners(PhotoQuad)
    }

    /// A believable page outline: convex, clockwise, big enough, inside the image and without
    /// extreme angles.
    static func isPlausible(_ quad: PhotoQuad, imageSize: CGSize) -> Bool {
        let w = Double(imageSize.width)
        let h = Double(imageSize.height)
        guard w > 0, h > 0, quad.isConvexClockwise, quad.area >= minimumAreaFraction * w * h else { return false }
        let slack = 0.02 * max(w, h)
        for p in quad.points {
            let x = Double(p.x)
            let y = Double(p.y)
            if x < -slack || y < -slack || x > w + slack || y > h + slack { return false }
        }
        return quad.interiorAngles.allSatisfy { $0 >= 25 && $0 <= 155 }
    }

    static func triage(_ detected: PhotoQuad?, imageSize: CGSize) -> Triage {
        guard let quad = detected, isPlausible(quad, imageSize: imageSize) else { return .asIs }
        return fillsFrame(quad, imageSize: imageSize) ? .asIs : .needsCorners(quad)
    }

    /// The outline is (nearly) the whole image: it covers at least `asIsCoverage` of it with every
    /// corner within `asIsCornerDistance` of the image's own corner.
    static func fillsFrame(_ quad: PhotoQuad, imageSize: CGSize) -> Bool {
        let w = Double(imageSize.width)
        let h = Double(imageSize.height)
        guard w > 0, h > 0 else { return false }
        let coverage = quad.area / (w * h)
        guard coverage >= asIsCoverage else { return false }
        let reach = asIsCornerDistance * (w * w + h * h).squareRoot()
        return zip(quad.points, PhotoQuad.full(imageSize).points).allSatisfy { distance($0, $1) <= reach }
    }

    /// Corners within `tolerance` (fraction of the long side) of the image corners: nothing to flatten.
    static func isFullImage(_ quad: PhotoQuad, imageSize: CGSize, tolerance: Double = 0.005) -> Bool {
        let reach = tolerance * Double(max(imageSize.width, imageSize.height))
        return zip(quad.points, PhotoQuad.full(imageSize).points).allSatisfy { distance($0, $1) <= reach }
    }

    // MARK: Homographies

    /// The projective transform taking `src[i]` to `dst[i]` exactly (four correspondences, no three
    /// collinear). Coordinates are normalised first (Hartley) for a well-conditioned solve.
    static func homography(from src: [CGPoint], to dst: [CGPoint]) -> Homography? {
        guard src.count == 4, dst.count == 4 else { return nil }
        guard let ns = normalization(src), let nd = normalization(dst) else { return nil }
        let s = src.map { apply(ns, $0) }
        let d = dst.map { apply(nd, $0) }
        // A·h = b for h = (h11 … h32), h33 = 1.
        var a = [[Double]](repeating: [Double](repeating: 0, count: 9), count: 8)
        for i in 0..<4 {
            let x = Double(s[i].x)
            let y = Double(s[i].y)
            let u = Double(d[i].x)
            let v = Double(d[i].y)
            a[2 * i] = [x, y, 1, 0, 0, 0, -u * x, -u * y, u]
            a[2 * i + 1] = [0, 0, 0, x, y, 1, -v * x, -v * y, v]
        }
        guard let h = solve8(a) else { return nil }
        let normalized = Homography([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1])
        // H = Nd⁻¹ · Hn · Ns
        guard let ndInverse = nd.inverse else { return nil }
        return scaled(compose(ndInverse, compose(normalized, ns)))
    }

    /// `a ∘ b`: apply `b` first, then `a`.
    static func compose(_ a: Homography, _ b: Homography) -> Homography {
        var m = [Double](repeating: 0, count: 9)
        for r in 0..<3 {
            for c in 0..<3 {
                m[3 * r + c] = a.m[3 * r] * b.m[c] + a.m[3 * r + 1] * b.m[3 + c] + a.m[3 * r + 2] * b.m[6 + c]
            }
        }
        return Homography(m)
    }

    /// Photo pixels → pixels of the page flattened into `width` × `height`.
    static func rectification(quad: PhotoQuad, width: Int, height: Int) -> Homography? {
        let w = CGFloat(width)
        let h = CGFloat(height)
        return homography(from: quad.points,
                          to: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)])
    }

    static func apply(_ h: Homography, _ p: CGPoint) -> CGPoint {
        let q = h.apply(SIMD2(Double(p.x), Double(p.y)))
        return CGPoint(x: q.x, y: q.y)
    }

    /// Divide by m33 (when it is not ~0) so equal transforms compare equal-ish.
    private static func scaled(_ h: Homography) -> Homography {
        let w = h.m[8]
        guard abs(w) > 1e-12 else { return h }
        return Homography(h.m.map { $0 / w })
    }

    /// Similarity moving the points' centroid to the origin with mean distance √2.
    private static func normalization(_ points: [CGPoint]) -> Homography? {
        let n = Double(points.count)
        let cx = points.reduce(0.0) { $0 + Double($1.x) } / n
        let cy = points.reduce(0.0) { $0 + Double($1.y) } / n
        let mean = points.reduce(0.0) { $0 + distance($1, CGPoint(x: cx, y: cy)) } / n
        guard mean.isFinite, mean > 1e-12 else { return nil }
        let s = 2.0.squareRoot() / mean
        return Homography([s, 0, -s * cx, 0, s, -s * cy, 0, 0, 1])
    }

    /// Gaussian elimination with partial pivoting on an 8 × 9 augmented matrix.
    private static func solve8(_ matrix: [[Double]]) -> [Double]? {
        var a = matrix
        let n = 8
        for col in 0..<n {
            var pivot = col
            for r in (col + 1)..<n where abs(a[r][col]) > abs(a[pivot][col]) {
                pivot = r
            }
            guard abs(a[pivot][col]) > 1e-12 else { return nil }
            if pivot != col { a.swapAt(pivot, col) }
            for r in (col + 1)..<n {
                let f = a[r][col] / a[col][col]
                if f == 0 { continue }
                for c in col...n {
                    a[r][c] -= f * a[col][c]
                }
            }
        }
        var x = [Double](repeating: 0, count: n)
        for r in stride(from: n - 1, through: 0, by: -1) {
            var sum = a[r][n]
            for c in (r + 1)..<n {
                sum -= a[r][c] * x[c]
            }
            x[r] = sum / a[r][r]
            guard x[r].isFinite else { return nil }
        }
        return x
    }

    // MARK: Flattened size

    /// Focal length of a typical phone's 1× lens (26 mm equivalent) as a fraction of the long side.
    static let defaultFocalFraction = 0.75
    /// Diagonal of a 35 mm frame (mm), to turn a 35 mm-equivalent focal length into pixels.
    static let fullFrameDiagonal = 43.27

    /// Width ÷ height of the real page whose perspective image is `quad`, for a pinhole camera with
    /// its principal point at the image centre (Zhang & He, "Whiteboard scanning and image
    /// enhancement", 2007). The focal length is solved from the page's two vanishing points when
    /// both are well defined, otherwise taken from `focalLength35mm` (EXIF) or a typical phone lens.
    /// Nil when the result is not trustworthy (degenerate quad, or far from the edge-length ratio,
    /// e.g. a cropped photo); callers then fall back to the edge-length ratio.
    static func pageAspect(of quad: PhotoQuad, imageSize: CGSize, focalLength35mm: Double? = nil) -> Double? {
        let w = Double(imageSize.width)
        let h = Double(imageSize.height)
        guard w > 0, h > 0 else { return nil }
        let long = max(w, h)
        let u0 = w / 2
        let v0 = h / 2
        let m1 = homogeneous(quad.topLeft)
        let m2 = homogeneous(quad.topRight)
        let m3 = homogeneous(quad.bottomLeft)
        let m4 = homogeneous(quad.bottomRight)
        let c14 = cross(m1, m4)
        let d2 = dot(cross(m2, m4), m3)
        let d3 = dot(cross(m3, m4), m2)
        guard abs(d2) > 1e-12, abs(d3) > 1e-12 else { return nil }
        let k2 = dot(c14, m3) / d2
        let k3 = dot(c14, m2) / d3
        // Images of the page's horizontal and vertical directions (up to scale).
        let n2 = k2 * m2 - m1
        let n3 = k3 * m3 - m1
        let xy2 = (n2.x * n2.x + n2.y * n2.y).squareRoot()
        let xy3 = (n3.x * n3.x + n3.y * n3.y).squareRoot()
        guard xy2 > 0, xy3 > 0 else { return nil }

        var focal: Double?
        // Both vanishing points finite (not ~parallel edges): solve for the focal length.
        if abs(n2.z) * long / xy2 >= 0.1, abs(n3.z) * long / xy3 >= 0.1 {
            let zz = n2.z * n3.z
            let fx = n2.x * n3.x - (n2.x * n3.z + n2.z * n3.x) * u0 + zz * u0 * u0
            let fy = n2.y * n3.y - (n2.y * n3.z + n2.z * n3.y) * v0 + zz * v0 * v0
            let f2 = -(fx + fy) / zz
            if f2.isFinite, f2 > 0 {
                let f = f2.squareRoot()
                if f >= 0.3 * long, f <= 5 * long { focal = f }
            }
        }
        if focal == nil {
            if let f35 = focalLength35mm, f35 > 0 {
                focal = f35 / fullFrameDiagonal * (w * w + h * h).squareRoot()
            } else {
                focal = defaultFocalFraction * long
            }
        }
        guard let f = focal, f > 0 else { return nil }
        let wx = (n2.x - u0 * n2.z) / f, wy = (n2.y - v0 * n2.z) / f
        let hx = (n3.x - u0 * n3.z) / f, hy = (n3.y - v0 * n3.z) / f
        let width2 = wx * wx + wy * wy + n2.z * n2.z
        let height2 = hx * hx + hy * hy + n3.z * n3.z
        guard width2 > 0, height2 > 0 else { return nil }
        let aspect = (width2 / height2).squareRoot()
        let edges = quad.edgeLengths
        let edgeWidth = max(edges.top, edges.bottom)
        let edgeHeight = max(edges.left, edges.right)
        guard aspect.isFinite, aspect > 0, edgeWidth > 0, edgeHeight > 0 else { return nil }
        // Far from the plain edge ratio: the camera assumptions do not hold (cropped photo, bad corners).
        guard abs(log(aspect / (edgeWidth / edgeHeight))) <= log(1.8) else { return nil }
        return aspect
    }

    /// Pixel size of the flattened page: the longer of each pair of opposite edges (so no detail is
    /// lost), stretched to the page's estimated aspect ratio, then capped at `maxLongSide` and `maxPixels`.
    static func outputSize(for quad: PhotoQuad, imageSize: CGSize, focalLength35mm: Double? = nil,
                           maxLongSide: Double = 6000, maxPixels: Double = 36_000_000) -> (width: Int, height: Int) {
        let edges = quad.edgeLengths
        var w = max(edges.top, edges.bottom, 1)
        var h = max(edges.left, edges.right, 1)
        if let aspect = pageAspect(of: quad, imageSize: imageSize, focalLength35mm: focalLength35mm) {
            if w / h >= aspect {
                h = w / aspect
            } else {
                w = h * aspect
            }
        }
        var s = min(1, maxLongSide / max(w, h))
        s = min(s, (maxPixels / (w * h)).squareRoot())
        return (max(1, Int((w * s).rounded())), max(1, Int((h * s).rounded())))
    }

    private static func homogeneous(_ p: CGPoint) -> SIMD3<Double> { SIMD3(Double(p.x), Double(p.y), 1) }

    private static func cross(_ a: SIMD3<Double>, _ b: SIMD3<Double>) -> SIMD3<Double> {
        SIMD3(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
    }

    private static func dot(_ a: SIMD3<Double>, _ b: SIMD3<Double>) -> Double {
        a.x * b.x + a.y * b.y + a.z * b.z
    }
}

enum PhotoRectifyError: Error, LocalizedError {
    case invalidCorners
    case renderFailed

    var errorDescription: String? {
        switch self {
        case .invalidCorners: return "The page corners don't form a four-sided shape."
        case .renderFailed: return "The photo could not be flattened."
        }
    }
}

/// Flattens the page of a photo with Core Image (any thread).
enum MapPhotoRectifier {
    private static let context = CIContext(options: [.cacheIntermediates: false])

    /// The page inside `quad` (pixels of `image`, top-left origin) warped to a rectangle of
    /// `PhotoGeometry.outputSize`, as an opaque sRGB raster. The result is exactly
    /// `PhotoGeometry.rectification(quad:width:height:)` applied to the photo.
    static func flatten(_ image: CGImage, quad: PhotoQuad, focalLength35mm: Double? = nil,
                        maxLongSide: Double = 6000) throws -> CGImage {
        let imageSize = CGSize(width: image.width, height: image.height)
        guard quad.isConvexClockwise, quad.area >= 16 else { throw PhotoRectifyError.invalidCorners }
        let size = PhotoGeometry.outputSize(for: quad, imageSize: imageSize, focalLength35mm: focalLength35mm,
                                            maxLongSide: maxLongSide)
        let imageHeight = CGFloat(image.height)
        guard let filter = CIFilter(name: "CIPerspectiveCorrection") else { throw PhotoRectifyError.renderFailed }
        filter.setValue(CIImage(cgImage: image), forKey: kCIInputImageKey)
        // Core Image's origin is bottom-left: the page's top corners have the larger y there.
        let corners: [(String, CGPoint)] = [
            ("inputTopLeft", quad.topLeft), ("inputTopRight", quad.topRight),
            ("inputBottomRight", quad.bottomRight), ("inputBottomLeft", quad.bottomLeft),
        ]
        for (key, point) in corners {
            let p = PhotoGeometry.coreImagePoint(fromPixel: point, imageHeight: imageHeight)
            filter.setValue(CIVector(cgPoint: p), forKey: key)
        }
        guard let corrected = filter.outputImage else { throw PhotoRectifyError.renderFailed }
        let extent = corrected.extent
        guard extent.width >= 1, extent.height >= 1, extent.width.isFinite, extent.height.isFinite else {
            throw PhotoRectifyError.renderFailed
        }
        // The corrected page fills its extent; move it to the origin and stretch it to the target size.
        let width = CGFloat(size.width)
        let height = CGFloat(size.height)
        let place = CGAffineTransform(translationX: -extent.minX, y: -extent.minY)
            .concatenating(CGAffineTransform(scaleX: width / extent.width, y: height / extent.height))
        let target = CGRect(x: 0, y: 0, width: width, height: height)
        let paper = CIImage(color: CIColor(red: 1, green: 1, blue: 1)).cropped(to: target)
        let output = corrected.transformed(by: place).composited(over: paper)
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let flat = context.createCGImage(output, from: target, format: .RGBA8, colorSpace: space)
        else { throw PhotoRectifyError.renderFailed }
        return flat
    }

    /// Opaque downscaled copy with the long side ≤ `maxLongSide` (the image itself when smaller).
    static func downscaled(_ image: CGImage, maxLongSide: Int) -> CGImage? {
        let long = max(image.width, image.height)
        guard long > maxLongSide else { return image }
        let s = Double(maxLongSide) / Double(long)
        let w = max(1, Int((Double(image.width) * s).rounded()))
        let h = max(1, Int((Double(image.height) * s).rounded()))
        guard let ctx = TileStore.makeContext(width: w, height: h) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage()
    }
}
