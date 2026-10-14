import CoreGraphics
import Foundation
import XCTest
@testable import MapMe

/// How MapImporter's 300 dpi PDF render resamples a raster embedded at a lower resolution. A Gaia
/// GPS export is one ~100 ppi map image without /Interpolate on an A4 page, so the render scales
/// it about 3×: nearest-neighbour blocks there would turn every thin contour line into a staircase.
/// The test builds such a PDF by hand (a 1 px "contour line" and a step edge in a 100 ppi image),
/// renders it like an import and checks that the upscaling is smooth and unbiased. It also
/// attaches the measured profile across the line next to the profiles nearest-neighbour,
/// bilinear, bicubic and Lanczos resampling would give, so CI records which kernel CoreGraphics
/// used (the test corpus renders its PDFs to match).
final class PDFRasterRenderTests: XCTestCase {
    /// 7 in square: 2100 px at 300 dpi, within MapImporter's size limits.
    private static let pagePt = 504.0
    private static let imageWidth = 60
    private static let imageHeight = 20
    /// 100 ppi: 0.72 pt per image pixel, 3 render pixels per image pixel.
    private static let ptPerPixel = 0.72
    /// Image origin on the page (bottom-left, points): 300 render pixels in from both edges.
    private static let originPt = 72.0
    /// The 1 px line's column and the first column of the dark area, in image pixels.
    private static let lineColumn = 30
    private static let edgeColumn = 45

    func testEmbeddedLowResolutionRasterIsUpscaledSmoothly() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("raster-\(UUID().uuidString).pdf")
        defer { try? FileManager.default.removeItem(at: url) }
        try Self.rasterPDF().write(to: url)

        let (image, dpi) = try MapImporter.renderPDF(at: url)
        XCTAssertEqual(dpi, 300, accuracy: 1e-9)
        XCTAssertEqual(image.width, 2100)
        XCTAssertEqual(image.height, 2100)
        let gray = try XCTUnwrap(Self.grayPixels(image))

        // Render pixels: the image spans columns 300..<480 and rows 1740..<1800 (top-left origin).
        let scale = 3.0
        let left = Int(Self.originPt / 72 * 300)
        let top = image.height - Int((Self.originPt + Double(Self.imageHeight) * Self.ptPerPixel) / 72 * 300)
        let row = top + Self.imageHeight * 3 / 2
        let profile = (0..<image.width).map { Double(gray[row * image.width + $0]) }

        // Across the line: image columns 20..<40 → render columns 360..<420.
        let span = (left + 20 * 3)..<(left + 40 * 3)
        let values = span.map { profile[$0] }
        let paper = values.max() ?? 255
        let ink = values.min() ?? 0
        XCTAssertGreaterThan(paper - ink, 60, "the line is lost (paper \(paper), ink \(ink))")
        let range = max(paper - ink, 1)
        let darkness = span.map { (paper - profile[$0]) / range }
        let intermediate = darkness.filter { $0 > 0.1 && $0 < 0.9 }.count

        // Where the line's ink lies: the centre of image column 30 is render x = 300 + 30.5 × 3,
        // the middle of render column 391.
        let mass = darkness.reduce(0, +)
        let centroid = zip(span, darkness).reduce(0.0) { $0 + Double($1.0) * $1.1 } / max(mass, 1e-9)
        let expectedCentre = Double(left) + (Double(Self.lineColumn) + 0.5) * scale - 0.5

        let report = Self.kernelReport(darkness: darkness, firstColumn: span.lowerBound, left: left, scale: scale)
        let attachment = XCTAttachment(string: report + "\ncentroid \(centroid), expected \(expectedCentre); intermediate pixels \(intermediate)")
        attachment.name = "pdf-raster-upscaling"
        attachment.lifetime = .keepAlways
        add(attachment)
        print(report)

        XCTAssertGreaterThanOrEqual(intermediate, 2,
                                    "CoreGraphics drew the 100 ppi raster nearest-neighbour at 300 dpi (no smoothing):\n\(report)")
        XCTAssertEqual(centroid, expectedCentre, accuracy: 0.75, "the upscaled line moved:\n\(report)")
        // The ink stays on the line: what darkens the render lies within ±2 image pixels of it.
        let outside = zip(span, darkness).filter { abs(Double($0.0) - expectedCentre) > 2 * scale }.map { $0.1 }.max() ?? 0
        XCTAssertLessThan(outside, 0.15, "ink spreads more than 2 image pixels from the line:\n\(report)")

        // The step edge stays where it was: half-way at the boundary between image columns 44 and 45.
        let edgeX = Double(left) + Double(Self.edgeColumn) * scale
        let stepPaper = profile[left + (Self.edgeColumn - 6) * 3]
        let stepInk = profile[left + (Self.edgeColumn + 6) * 3]
        let half = (stepPaper + stepInk) / 2
        let crossing = ((left + (Self.edgeColumn - 6) * 3)..<(left + (Self.edgeColumn + 6) * 3)).first { profile[$0] <= half }
        if let crossing {
            XCTAssertEqual(Double(crossing), edgeX, accuracy: 2, "the step edge moved")
        } else {
            XCTFail("no step edge in the render")
        }
    }

    // MARK: - Helpers

    /// A one-page PDF with a 60 × 20 px DeviceRGB image (8 bits, uncompressed, no /Interpolate)
    /// drawn at 100 ppi: paper white, a 1 px brown line at column 30, dark grey from column 45.
    static func rasterPDF() -> Data {
        var samples: [UInt8] = []
        for _ in 0..<imageHeight {
            for x in 0..<imageWidth {
                if x == lineColumn {
                    samples += [110, 70, 40]
                } else if x >= edgeColumn {
                    samples += [60, 60, 60]
                } else {
                    samples += [255, 255, 255]
                }
            }
        }
        func number(_ v: Double) -> String { String(format: "%.4f", v) }
        let w = Double(imageWidth) * ptPerPixel
        let h = Double(imageHeight) * ptPerPixel
        let content = "q \(number(w)) 0 0 \(number(h)) \(number(originPt)) \(number(originPt)) cm /I0 Do Q\n"

        var pdf = Data("%PDF-1.4\n".utf8)
        pdf.append(contentsOf: [0x25, 0xE2, 0xE3, 0xCF, 0xD3, 0x0A])
        var offsets: [Int] = []
        func object(_ body: String, stream: Data? = nil) {
            offsets.append(pdf.count)
            pdf.append(Data("\(offsets.count) 0 obj\n\(body)\n".utf8))
            if let stream {
                pdf.append(Data("stream\n".utf8))
                pdf.append(stream)
                pdf.append(Data("\nendstream\n".utf8))
            }
            pdf.append(Data("endobj\n".utf8))
        }
        object("<< /Type /Catalog /Pages 2 0 R >>")
        object("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        object("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 \(number(pagePt)) \(number(pagePt))] "
            + "/Resources << /XObject << /I0 4 0 R >> >> /Contents 5 0 R >>")
        object("<< /Type /XObject /Subtype /Image /Width \(imageWidth) /Height \(imageHeight) /ColorSpace /DeviceRGB "
            + "/BitsPerComponent 8 /Length \(samples.count) >>", stream: Data(samples))
        object("<< /Length \(content.utf8.count) >>", stream: Data(content.utf8))
        let xref = pdf.count
        var table = "xref\n0 \(offsets.count + 1)\n0000000000 65535 f \n"
        for offset in offsets {
            table += String(format: "%010ld 00000 n \n", offset)
        }
        table += "trailer\n<< /Size \(offsets.count + 1) /Root 1 0 R >>\nstartxref\n\(xref)\n%%EOF\n"
        pdf.append(Data(table.utf8))
        return pdf
    }

    /// 8-bit grey copy of `image`, top row first.
    static func grayPixels(_ image: CGImage) -> [UInt8]? {
        let w = image.width
        let h = image.height
        var pixels = [UInt8](repeating: 0, count: w * h)
        let drawn = pixels.withUnsafeMutableBytes { raw -> Bool in
            guard let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                                      space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)
            else { return false }
            ctx.interpolationQuality = .none
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        return drawn ? pixels : nil
    }

    /// The measured darkness across the line next to what common kernels give for a 1 px line
    /// upscaled `scale`× with pixel centres aligned, and the closest of them.
    static func kernelReport(darkness: [Double], firstColumn: Int, left: Int, scale: Double) -> String {
        let kernels: [(String, (Double) -> Double)] = [
            ("nearest", { abs($0) < 0.5 ? 1 : 0 }),
            ("bilinear", { max(0, 1 - abs($0)) }),
            ("bicubic a=-0.5 (Catmull-Rom)", { Self.cubic($0, a: -0.5) }),
            ("bicubic a=-0.75 (OpenCV INTER_CUBIC)", { Self.cubic($0, a: -0.75) }),
            ("lanczos3", { Self.lanczos($0, lobes: 3) }),
        ]
        let line = Double(lineColumn)
        var lines = ["column  measured  " + kernels.map { $0.0 }.joined(separator: " | ")]
        var errors = [Double](repeating: 0, count: kernels.count)
        for (i, d) in darkness.enumerated() {
            let column = firstColumn + i
            // Source coordinate of this render pixel's centre.
            let u = (Double(column - left) + 0.5) / scale - 0.5
            let predicted = kernels.map { $0.1(u - line) }
            for k in kernels.indices {
                errors[k] += (predicted[k] - d) * (predicted[k] - d)
            }
            if abs(u - line) < 3 {
                lines.append("\(column)  " + String(format: "%.3f  ", d) + predicted.map { String(format: "%.3f", $0) }.joined(separator: " "))
            }
        }
        let rms = errors.map { ($0 / Double(max(darkness.count, 1))).squareRoot() }
        let best = rms.indices.min { rms[$0] < rms[$1] } ?? 0
        lines.append("rms: " + kernels.indices.map { "\(kernels[$0].0) \(String(format: "%.3f", rms[$0]))" }.joined(separator: ", "))
        lines.append("closest: \(kernels[best].0)")
        return "PDF raster upscaling (100 ppi image rendered at 300 dpi, interpolationQuality .high):\n"
            + lines.joined(separator: "\n")
    }

    private static func cubic(_ x: Double, a: Double) -> Double {
        let t = abs(x)
        if t <= 1 { return (a + 2) * t * t * t - (a + 3) * t * t + 1 }
        if t < 2 { return a * t * t * t - 5 * a * t * t + 8 * a * t - 4 * a }
        return 0
    }

    private static func lanczos(_ x: Double, lobes: Double) -> Double {
        if x == 0 { return 1 }
        guard abs(x) < lobes else { return 0 }
        let px = Double.pi * x
        return lobes * sin(px) * sin(px / lobes) / (px * px)
    }
}
