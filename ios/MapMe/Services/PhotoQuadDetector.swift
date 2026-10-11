import CoreGraphics
import Foundation
import Vision

/// Finds the paper in a photo: Vision's document segmentation (rectangle detection as a fallback)
/// for the rough outline, then each edge is snapped to the paper's border in the pixels and the
/// corners are recomputed as the intersections of the fitted edge lines. Vision's corners come from
/// a low-resolution pass and are typically off by ~1 % of the frame; the snapped ones are within a
/// pixel or two, which keeps the flattened map free of residual perspective.
enum PhotoQuadDetector {
    /// Long side of the downscaled copy Vision and the edge snapping work on.
    static let workingLongSide = 1600

    /// The page outline in pixels of `image` (top-left origin), or nil when there is no plausible page.
    /// Runs off the calling thread.
    static func detect(in image: CGImage) async -> PhotoQuad? {
        await withCheckedContinuation { (continuation: CheckedContinuation<PhotoQuad?, Never>) in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: detectSync(in: image))
            }
        }
    }

    static func detectSync(in image: CGImage) -> PhotoQuad? {
        guard image.width > 0, image.height > 0,
              let small = MapPhotoRectifier.downscaled(image, maxLongSide: workingLongSide)
        else { return nil }
        let smallSize = CGSize(width: small.width, height: small.height)
        let candidates = visionQuads(in: small)
        guard let rough = candidates.max(by: { $0.area < $1.area }) else { return nil }
        var quad = rough
        if let gray = GrayImage(small) {
            let snapped = refine(rough, in: gray)
            if PhotoGeometry.isPlausible(snapped, imageSize: smallSize) { quad = snapped }
        }
        let sx = CGFloat(image.width) / smallSize.width
        let sy = CGFloat(image.height) / smallSize.height
        return quad.scaled(x: sx, y: sy).clamped(to: CGSize(width: image.width, height: image.height))
    }

    // MARK: - Vision

    /// Plausible page outlines (pixels of `image`): document segmentation first, rectangles if it
    /// finds nothing.
    static func visionQuads(in image: CGImage) -> [PhotoQuad] {
        let size = CGSize(width: image.width, height: image.height)
        let handler = VNImageRequestHandler(cgImage: image, orientation: .up, options: [:])

        let segmentation = VNDetectDocumentSegmentationRequest()
        #if targetEnvironment(simulator)
        // The simulator has no Neural Engine; Vision needs the CPU path there.
        segmentation.usesCPUOnly = true
        #endif
        var found: [PhotoQuad] = []
        do {
            try handler.perform([segmentation])
            found = (segmentation.results ?? []).compactMap { quad(from: $0, imageSize: size) }
        } catch {
            #if DEBUG
            print("Document segmentation failed: \(error)")
            #endif
        }
        if !found.isEmpty { return found }

        let rectangles = VNDetectRectanglesRequest()
        rectangles.minimumAspectRatio = 0.2
        rectangles.maximumAspectRatio = 1.0
        rectangles.minimumSize = 0.25
        rectangles.minimumConfidence = 0.3
        rectangles.quadratureTolerance = 40
        rectangles.maximumObservations = 8
        do {
            try handler.perform([rectangles])
            found = (rectangles.results ?? []).compactMap { quad(from: $0, imageSize: size) }
        } catch {
            #if DEBUG
            print("Rectangle detection failed: \(error)")
            #endif
        }
        return found
    }

    /// Vision's corners (normalised, origin bottom-left) → an ordered, plausible quad in pixels.
    static func quad(from observation: VNRectangleObservation, imageSize: CGSize) -> PhotoQuad? {
        let corners = [observation.topLeft, observation.topRight, observation.bottomRight, observation.bottomLeft]
            .map { PhotoGeometry.pixel(fromVision: $0, imageSize: imageSize) }
        guard let quad = PhotoQuad.ordered(corners), PhotoGeometry.isPlausible(quad, imageSize: imageSize) else { return nil }
        return quad
    }

    // MARK: - Edge snapping (pure)

    /// How far (fraction of the long side) each edge is searched for the paper border.
    static let searchFraction = 0.025
    /// Edge sample positions per side.
    static let samplesPerEdge = 48
    /// Half-width (px) of the two boxes compared across the border.
    static let boxWidth = 5
    /// Smallest brightness step (0…1) that counts as a border.
    static let minimumStep: Float = 0.08

    /// A straight line through `point` along the unit vector `direction`.
    struct Line {
        var point: CGPoint
        var direction: CGVector
    }

    /// Snap each edge of `quad` (pixels of `gray`) to the strongest straight brightness step near it
    /// and intersect neighbouring edges. Corners whose edges cannot be found keep their position.
    static func refine(_ quad: PhotoQuad, in gray: GrayImage) -> PhotoQuad {
        let corners = quad.points
        let reach = max(8, searchFraction * Double(max(gray.width, gray.height)))
        let center = quad.centroid
        var lines: [Line?] = []
        for i in 0..<4 {
            lines.append(edgeLine(from: corners[i], to: corners[(i + 1) % 4], inward: center, reach: reach, in: gray))
        }
        var refined = corners
        for i in 0..<4 {
            // Corner i joins the edge arriving at it (i − 1) and the edge leaving it (i).
            guard let incoming = lines[(i + 3) % 4], let outgoing = lines[i],
                  let p = intersection(incoming, outgoing)
            else { continue }
            if PhotoGeometry.distance(p, corners[i]) <= 1.5 * reach {
                refined[i] = p
            }
        }
        guard let result = PhotoQuad(points: refined), result.isConvexClockwise else { return quad }
        return result
    }

    /// Fit the paper border near the edge a → b: sample the brightness across the edge at evenly
    /// spaced points, take the position of the largest step between two small boxes (outside vs.
    /// inside), keep the steps of the majority sign and fit a line robustly.
    static func edgeLine(from a: CGPoint, to b: CGPoint, inward center: CGPoint, reach: Double, in gray: GrayImage) -> Line? {
        let dx = Double(b.x - a.x)
        let dy = Double(b.y - a.y)
        let length = (dx * dx + dy * dy).squareRoot()
        guard length > 4 else { return nil }
        let tx = dx / length
        let ty = dy / length
        var nx = -ty
        var ny = tx
        let mx = Double(a.x + b.x) / 2
        let my = Double(a.y + b.y) / 2
        if (Double(center.x) - mx) * nx + (Double(center.y) - my) * ny < 0 {
            nx = -nx
            ny = -ny
        }
        let k = boxWidth
        let steps = Int(reach.rounded())
        var hits: [(point: CGPoint, sign: Float)] = []
        for j in 0..<samplesPerEdge {
            let s = 0.08 + 0.84 * Double(j) / Double(samplesPerEdge - 1)
            let px = Double(a.x) + dx * s
            let py = Double(a.y) + dy * s
            // Brightness profile from outside (−) to inside (+), offsets −steps−k … steps+k.
            var profile: [Float] = []
            profile.reserveCapacity(2 * (steps + k) + 1)
            var missing = 0
            for o in -(steps + k)...(steps + k) {
                // Quad coordinates put pixel edges at integers; GrayImage puts pixel centres there.
                if let v = gray.sample(x: px + nx * Double(o) - 0.5, y: py + ny * Double(o) - 0.5) {
                    profile.append(v)
                } else {
                    profile.append(.nan)
                    missing += 1
                }
            }
            guard missing * 10 <= profile.count * 3 else { continue }
            let valid = profile.filter { !$0.isNaN }
            let fill = valid.reduce(0, +) / Float(max(1, valid.count))
            var prefix: [Float] = [0]
            prefix.reserveCapacity(profile.count + 1)
            for v in profile {
                prefix.append(prefix[prefix.count - 1] + (v.isNaN ? fill : v))
            }
            // Step response at offset o (profile index c = o + steps + k): mean(c+1…c+k) − mean(c−k…c−1).
            var bestIndex = -1
            var bestValue: Float = 0
            var responses: [Float] = []
            responses.reserveCapacity(2 * steps + 1)
            for idx in 0...(2 * steps) {
                let c = idx + k
                let inside = (prefix[c + k + 1] - prefix[c + 1]) / Float(k)
                let outside = (prefix[c] - prefix[c - k]) / Float(k)
                let r = inside - outside
                responses.append(r)
                if abs(r) > abs(bestValue) {
                    bestValue = r
                    bestIndex = idx
                }
            }
            guard bestIndex >= 0, abs(bestValue) >= minimumStep else { continue }
            var offset = Double(bestIndex - steps)
            if bestIndex > 0, bestIndex < responses.count - 1 {
                // Sub-pixel peak (parabola through the neighbours).
                let y0 = Double(abs(responses[bestIndex - 1]))
                let y1 = Double(abs(responses[bestIndex]))
                let y2 = Double(abs(responses[bestIndex + 1]))
                let den = y0 - 2 * y1 + y2
                if abs(den) > 1e-9 {
                    offset += min(max(0.5 * (y0 - y2) / den, -0.5), 0.5)
                }
            }
            let sign: Float = bestValue > 0 ? 1 : -1
            hits.append((point: CGPoint(x: px + nx * offset, y: py + ny * offset), sign: sign))
        }
        let positives = hits.filter { $0.sign > 0 }.count
        let majority: Float = positives * 2 >= hits.count ? 1 : -1
        let points = hits.filter { $0.sign == majority }.map { $0.point }
        let needed = Int((Double(samplesPerEdge) * 0.4).rounded(.up))
        guard points.count >= needed else { return nil }
        return robustLine(points, minimumInliers: Int((Double(samplesPerEdge) * 0.3).rounded(.up)))
    }

    /// Total-least-squares line, refitted a few times without points far from it.
    static func robustLine(_ points: [CGPoint], minimumInliers: Int) -> Line? {
        var current = points
        var line = fitLine(current)
        for _ in 0..<4 {
            guard let l = line else { return nil }
            let residuals = current.map { distanceToLine($0, l) }
            let sortedResiduals = residuals.sorted()
            let median = sortedResiduals[sortedResiduals.count / 2]
            let threshold = max(1.0, 2.5 * median)
            let kept = zip(current, residuals).filter { $0.1 <= threshold }.map { $0.0 }
            if kept.count < 6 || kept.count == current.count { break }
            current = kept
            line = fitLine(current)
        }
        guard current.count >= minimumInliers else { return nil }
        return line
    }

    /// Principal axis of the points.
    static func fitLine(_ points: [CGPoint]) -> Line? {
        guard points.count >= 2 else { return nil }
        let n = Double(points.count)
        let cx = points.reduce(0.0) { $0 + Double($1.x) } / n
        let cy = points.reduce(0.0) { $0 + Double($1.y) } / n
        var sxx = 0.0, sxy = 0.0, syy = 0.0
        for p in points {
            let u = Double(p.x) - cx
            let v = Double(p.y) - cy
            sxx += u * u
            sxy += u * v
            syy += v * v
        }
        guard sxx + syy > 1e-9 else { return nil }
        let theta = 0.5 * atan2(2 * sxy, sxx - syy)
        return Line(point: CGPoint(x: cx, y: cy), direction: CGVector(dx: cos(theta), dy: sin(theta)))
    }

    static func distanceToLine(_ p: CGPoint, _ line: Line) -> Double {
        let ux = Double(p.x - line.point.x)
        let uy = Double(p.y - line.point.y)
        return abs(ux * Double(line.direction.dy) - uy * Double(line.direction.dx))
    }

    static func intersection(_ l1: Line, _ l2: Line) -> CGPoint? {
        let d1x = Double(l1.direction.dx), d1y = Double(l1.direction.dy)
        let d2x = Double(l2.direction.dx), d2y = Double(l2.direction.dy)
        let denom = d1x * d2y - d1y * d2x
        guard abs(denom) > 1e-6 else { return nil }
        let wx = Double(l2.point.x - l1.point.x)
        let wy = Double(l2.point.y - l1.point.y)
        let s = (wx * d2y - wy * d2x) / denom
        return CGPoint(x: Double(l1.point.x) + s * d1x, y: Double(l1.point.y) + s * d1y)
    }
}

/// 8-bit grey copy of an image as floats 0…1, rows top to bottom.
struct GrayImage {
    let width: Int
    let height: Int
    let pixels: [Float]

    init?(_ image: CGImage) {
        let w = image.width
        let h = image.height
        guard w > 0, h > 0 else { return nil }
        var bytes = [UInt8](repeating: 0, count: w * h)
        let ok = bytes.withUnsafeMutableBytes { raw -> Bool in
            guard let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                                      space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)
            else { return false }
            ctx.interpolationQuality = .none
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard ok else { return nil }
        width = w
        height = h
        pixels = bytes.map { Float($0) / 255 }
    }

    init(width: Int, height: Int, pixels: [Float]) {
        self.width = width
        self.height = height
        self.pixels = pixels
    }

    /// Bilinear sample at a continuous position (pixel centres at integers); nil outside.
    func sample(x: Double, y: Double) -> Float? {
        guard x >= 0, y >= 0, x <= Double(width - 1), y <= Double(height - 1) else { return nil }
        let x0 = min(Int(x), max(width - 2, 0))
        let y0 = min(Int(y), max(height - 2, 0))
        let x1 = min(x0 + 1, width - 1)
        let y1 = min(y0 + 1, height - 1)
        let fx = Float(x - Double(x0))
        let fy = Float(y - Double(y0))
        let a = pixels[y0 * width + x0]
        let b = pixels[y0 * width + x1]
        let c = pixels[y1 * width + x0]
        let d = pixels[y1 * width + x1]
        return a * (1 - fx) * (1 - fy) + b * fx * (1 - fy) + c * (1 - fx) * fy + d * fx * fy
    }
}
