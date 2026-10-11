import CoreGraphics
import Foundation
import Vision

/// How a crop is turned before Vision reads it.
enum OCRRotation: Equatable {
    case upright
    /// Turned 90° clockwise: reads text running bottom-to-top (e.g. latitude labels on a left margin).
    case clockwise
    /// Turned 90° counter-clockwise: reads text running top-to-bottom.
    case counterClockwise
}

/// One Vision pass: a crop of the base image (base pixels, top-left origin), optionally rotated.
struct OCRJob: Equatable {
    var crop: CGRect
    var rotation: OCRRotation
}

enum OCRService {
    static let tileSize = 1600
    static let overlap = 0.15
    /// Fraction of each side scanned again with rotated passes for vertical labels.
    static let marginFraction = 0.18
    static let maxConcurrentPasses = 3
    /// Smallest text Vision should look for, in processed-image pixels.
    static let minimumTextPixels = 12.0

    /// Recognise text anywhere on the map, including vertical (rotated) grid labels.
    /// Returned boxes are in base-image pixels, top-left origin.
    static func recognize(in image: CGImage, progress: ((Double) -> Void)? = nil) async throws -> [OCRObservation] {
        let jobs = plan(width: image.width, height: image.height)
        guard !jobs.isEmpty else { return [] }
        var all: [OCRObservation] = []
        var done = 0
        try await withThrowingTaskGroup(of: [OCRObservation].self) { group in
            var pending = jobs.makeIterator()
            for _ in 0..<maxConcurrentPasses {
                guard let job = pending.next() else { break }
                group.addTask { try await run(job, on: image) }
            }
            while let found = try await group.next() {
                all.append(contentsOf: found)
                done += 1
                progress?(Double(done) / Double(jobs.count))
                try Task.checkCancellation()
                if let job = pending.next() {
                    group.addTask { try await run(job, on: image) }
                }
            }
        }
        return deduplicate(all)
    }

    // MARK: - Planning (pure)

    /// Upright overlapping tiles over the whole image, plus clockwise and counter-clockwise
    /// passes over the four margin strips (split along their length into tile-sized pieces).
    static func plan(width: Int, height: Int) -> [OCRJob] {
        guard width > 0, height > 0 else { return [] }
        var jobs = tileRects(width: width, height: height).map { OCRJob(crop: $0, rotation: .upright) }
        let mx = max(1, min(width, Int((Double(width) * marginFraction).rounded())))
        let my = max(1, min(height, Int((Double(height) * marginFraction).rounded())))
        let strips = [
            CGRect(x: 0, y: 0, width: mx, height: height),
            CGRect(x: width - mx, y: 0, width: mx, height: height),
            CGRect(x: 0, y: 0, width: width, height: my),
            CGRect(x: 0, y: height - my, width: width, height: my),
        ]
        for strip in strips {
            let pieces: [CGRect]
            if strip.height >= strip.width {
                pieces = spans(length: Int(strip.height)).map {
                    CGRect(x: strip.minX, y: strip.minY + CGFloat($0.start), width: strip.width, height: CGFloat($0.length))
                }
            } else {
                pieces = spans(length: Int(strip.width)).map {
                    CGRect(x: strip.minX + CGFloat($0.start), y: strip.minY, width: CGFloat($0.length), height: strip.height)
                }
            }
            for piece in pieces {
                jobs.append(OCRJob(crop: piece, rotation: .clockwise))
                jobs.append(OCRJob(crop: piece, rotation: .counterClockwise))
            }
        }
        return jobs
    }

    /// Overlapping tiles (at most `tile` px, at least `overlap` shared) covering width × height.
    static func tileRects(width: Int, height: Int, tile: Int = OCRService.tileSize,
                          overlap: Double = OCRService.overlap) -> [CGRect] {
        let xs = spans(length: width, tile: tile, overlap: overlap)
        let ys = spans(length: height, tile: tile, overlap: overlap)
        var rects: [CGRect] = []
        for y in ys {
            for x in xs {
                rects.append(CGRect(x: x.start, y: y.start, width: x.length, height: y.length))
            }
        }
        return rects
    }

    /// 1-D split of [0, length) into evenly spaced windows of `tile` px overlapping by ≥ `overlap`.
    static func spans(length: Int, tile: Int = OCRService.tileSize,
                      overlap: Double = OCRService.overlap) -> [(start: Int, length: Int)] {
        guard length > 0 else { return [] }
        guard length > tile else { return [(start: 0, length: length)] }
        let step = Double(tile) * (1 - overlap)
        let n = Int((Double(length - tile) / step).rounded(.up)) + 1
        return (0..<n).map { i in
            let start = Int((Double(i) * Double(length - tile) / Double(n - 1)).rounded())
            return (start: start, length: tile)
        }
    }

    // MARK: - Geometry (pure)

    /// Vision's normalised box (origin bottom-left) → pixels of the processed image (origin top-left).
    static func visionRectToPixels(_ box: CGRect, imageSize: CGSize) -> CGRect {
        CGRect(x: box.minX * imageSize.width,
               y: (1 - box.maxY) * imageSize.height,
               width: box.width * imageSize.width,
               height: box.height * imageSize.height)
    }

    /// A rect in the processed image (the crop, maybe rotated) → base-image pixels.
    /// Rotation maps crop pixel (u, v) to (h − v, u) for clockwise and (v, w − u) for
    /// counter-clockwise, w × h being the crop size; this applies the inverse.
    static func mapToBase(_ r: CGRect, crop: CGRect, rotation: OCRRotation) -> CGRect {
        let local: CGRect
        switch rotation {
        case .upright:
            local = r
        case .clockwise:
            local = CGRect(x: r.minY, y: crop.height - r.maxX, width: r.height, height: r.width)
        case .counterClockwise:
            local = CGRect(x: crop.width - r.maxY, y: r.minX, width: r.height, height: r.width)
        }
        return local.offsetBy(dx: crop.minX, dy: crop.minY)
    }

    /// The image turned by a quarter turn (exact pixel permutation, no resampling).
    static func rotated(_ image: CGImage, _ rotation: OCRRotation) -> CGImage? {
        let w = CGFloat(image.width)
        let h = CGFloat(image.height)
        let transform: CGAffineTransform
        switch rotation {
        case .upright:
            return image
        case .clockwise: // CoreGraphics (y-up): (x, y) → (y, w − x)
            transform = CGAffineTransform(a: 0, b: -1, c: 1, d: 0, tx: 0, ty: w)
        case .counterClockwise: // (x, y) → (h − y, x)
            transform = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: h, ty: 0)
        }
        guard let ctx = TileStore.makeContext(width: image.height, height: image.width) else { return nil }
        ctx.interpolationQuality = .none
        ctx.concatenate(transform)
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage()
    }

    // MARK: - De-duplication (pure)

    /// Merge reads of the same label from overlapping passes: same text with IoU > 0.4 keeps the
    /// more confident one; a partial read cut at a tile edge (text contained in the other and
    /// mostly overlapping it) keeps the longer text.
    static func deduplicate(_ observations: [OCRObservation]) -> [OCRObservation] {
        let ordered = observations.sorted {
            $0.confidence != $1.confidence ? $0.confidence > $1.confidence : $0.text.count > $1.text.count
        }
        var kept: [(obs: OCRObservation, key: String, rect: CGRect)] = []
        candidates: for o in ordered {
            let key = normalized(o.text)
            guard !key.isEmpty, o.bbox.count == 4 else { continue }
            let r = rect(o)
            for i in kept.indices {
                let k = kept[i]
                if k.key == key {
                    if iou(r, k.rect) > 0.4 { continue candidates }
                } else if (k.key.contains(key) || key.contains(k.key)) && overlapOfSmaller(r, k.rect) > 0.6 {
                    if key.count > k.key.count { kept[i] = (o, key, r) }
                    continue candidates
                }
            }
            kept.append((o, key, r))
        }
        return kept.map { $0.obs }.sorted { a, b in
            a.bbox[1] != b.bbox[1] ? a.bbox[1] < b.bbox[1] : a.bbox[0] < b.bbox[0]
        }
    }

    static func iou(_ a: CGRect, _ b: CGRect) -> Double {
        let i = a.intersection(b)
        guard !i.isNull, !i.isEmpty else { return 0 }
        let inter = Double(i.width * i.height)
        let union = Double(a.width * a.height + b.width * b.height) - inter
        return union > 0 ? inter / union : 0
    }

    private static func overlapOfSmaller(_ a: CGRect, _ b: CGRect) -> Double {
        let i = a.intersection(b)
        guard !i.isNull, !i.isEmpty else { return 0 }
        let smaller = Double(min(a.width * a.height, b.width * b.height))
        return smaller > 0 ? Double(i.width * i.height) / smaller : 0
    }

    private static func rect(_ o: OCRObservation) -> CGRect {
        CGRect(x: o.bbox[0], y: o.bbox[1], width: o.bbox[2], height: o.bbox[3])
    }

    private static func normalized(_ text: String) -> String {
        String(text.lowercased().filter { !$0.isWhitespace })
    }

    // MARK: - Vision

    private struct TextLine {
        var text: String
        /// Vision's normalised box, origin bottom-left.
        var box: CGRect
        var confidence: Float
    }

    private static func run(_ job: OCRJob, on image: CGImage) async throws -> [OCRObservation] {
        try Task.checkCancellation()
        let bounds = CGRect(x: 0, y: 0, width: image.width, height: image.height)
        let cropRect = job.crop.integral.intersection(bounds)
        guard !cropRect.isNull, cropRect.width >= 8, cropRect.height >= 8,
              let crop = image.cropping(to: cropRect),
              let processed = rotated(crop, job.rotation)
        else { return [] }
        let cropFrame = CGRect(x: cropRect.minX, y: cropRect.minY, width: CGFloat(crop.width), height: CGFloat(crop.height))
        let size = CGSize(width: processed.width, height: processed.height)
        let minHeight = Float(min(0.05, minimumTextPixels / Double(max(1, processed.height))))
        let lines: [TextLine]
        do {
            lines = try await recognizeText(in: processed, minimumTextHeight: minHeight)
        } catch {
            // One failed pass must not lose the others.
            if Task.isCancelled { throw CancellationError() }
            #if DEBUG
            print("OCR pass \(job.rotation) \(job.crop) failed: \(error)")
            #endif
            return []
        }
        let processedBounds = CGRect(origin: .zero, size: size)
        return lines.compactMap { (line: TextLine) -> OCRObservation? in
            let px = visionRectToPixels(line.box, imageSize: size).intersection(processedBounds)
            guard !px.isNull, px.width > 0, px.height > 0 else { return nil }
            let b = mapToBase(px, crop: cropFrame, rotation: job.rotation)
            return OCRObservation(text: line.text,
                                  bbox: [Double(b.minX), Double(b.minY), Double(b.width), Double(b.height)],
                                  confidence: line.confidence)
        }
    }

    private static func recognizeText(in image: CGImage, minimumTextHeight: Float) async throws -> [TextLine] {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<[TextLine], Error>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let request = VNRecognizeTextRequest()
                request.recognitionLevel = .accurate
                request.usesLanguageCorrection = false
                request.recognitionLanguages = ["en-US"]
                request.minimumTextHeight = minimumTextHeight
                #if targetEnvironment(simulator)
                // The simulator has no Neural Engine; Vision needs the CPU path there.
                request.usesCPUOnly = true
                #endif
                let handler = VNImageRequestHandler(cgImage: image, orientation: .up, options: [:])
                do {
                    try handler.perform([request])
                    let lines: [TextLine] = (request.results ?? []).compactMap { (observation: VNRecognizedTextObservation) -> TextLine? in
                        guard let best = observation.topCandidates(1).first else { return nil }
                        let text = best.string.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !text.isEmpty else { return nil }
                        return TextLine(text: text, box: observation.boundingBox, confidence: best.confidence)
                    }
                    continuation.resume(returning: lines)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
}
