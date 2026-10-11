import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

struct ImportedMap {
    var document: MapDocument
    /// Full-resolution base raster (tile level 0), for georeferencing.
    var baseImage: CGImage
}

enum ImportError: Error, LocalizedError {
    case unreadable(String)
    case unsupported(String)
    var errorDescription: String? {
        switch self {
        case .unreadable(let s): return "Could not read the file: \(s)"
        case .unsupported(let s): return "Unsupported file: \(s)"
        }
    }
}

/// How a PDF page is rasterised: output size, effective dpi and the page → bitmap transform
/// (CoreGraphics bitmap space, origin bottom-left). The page is anchored at the top-left
/// pixel so that page content keeps an exact pixel grid.
struct PDFRenderPlan: Equatable {
    var width: Int
    var height: Int
    var scale: CGFloat
    var transform: CGAffineTransform

    var dpi: Double { 72 * Double(scale) }
}

/// Turns a PDF/image into a stored MapDocument + tile pyramid + thumbnail.
final class MapImporter {
    let library: MapLibrary

    static let pdfDPI: CGFloat = 300
    static let maxLongSide: CGFloat = 6000
    static let maxPixels: CGFloat = 36_000_000
    static let minLongSide: CGFloat = 2048
    static let thumbnailLongSide = 640

    init(library: MapLibrary) { self.library = library }

    /// `progress(fraction 0…1, step description)` may be called from any thread.
    func importFile(at url: URL, progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        progress(0.01, "Copying file…")
        let id = UUID()
        let folder = library.directory(for: id)
        let kind: MapSourceKind
        let destination: URL
        do {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            kind = try Self.sourceKind(of: url)
            destination = folder.appendingPathComponent("source." + Self.fileExtension(for: url, kind: kind))
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try Self.coordinatedCopy(from: url, to: destination)
        } catch {
            try? FileManager.default.removeItem(at: folder)
            throw error
        }
        Self.removeInboxCopy(url)
        return try await process(id: id, name: Self.displayName(for: url), source: destination, kind: kind, progress: progress)
    }

    func importImageData(_ data: Data, suggestedName: String, progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        progress(0.01, "Copying image…")
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let type = CGImageSourceGetType(source)
        else { throw ImportError.unsupported(suggestedName) }
        let ext = UTType(type as String)?.preferredFilenameExtension ?? "jpg"
        let id = UUID()
        let folder = library.directory(for: id)
        let destination = folder.appendingPathComponent("source.\(ext)")
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try data.write(to: destination, options: .atomic)
        } catch {
            try? FileManager.default.removeItem(at: folder)
            throw ImportError.unreadable(error.localizedDescription)
        }
        let trimmed = suggestedName.trimmingCharacters(in: .whitespacesAndNewlines)
        return try await process(id: id, name: trimmed.isEmpty ? "Untitled map" : trimmed, source: destination,
                                 kind: .image, progress: progress)
    }

    /// Re-create the base raster of an existing map (for re-locating).
    func loadBaseImage(for map: MapDocument) async throws -> CGImage {
        let url = library.sourceURL(for: map)
        let image = try await Task.detached(priority: .userInitiated) { () throws -> CGImage in
            let (rendered, _) = try MapImporter.renderBaseImage(from: url)
            guard rendered.width != map.pixelWidth || rendered.height != map.pixelHeight else { return rendered }
            // Keep pixel coordinates consistent with the stored tiles and georeference.
            guard let ctx = TileStore.makeContext(width: map.pixelWidth, height: map.pixelHeight) else {
                throw TileStoreError.contextFailed
            }
            ctx.interpolationQuality = .high
            ctx.draw(rendered, in: CGRect(x: 0, y: 0, width: map.pixelWidth, height: map.pixelHeight))
            guard let resized = ctx.makeImage() else { throw TileStoreError.contextFailed }
            return resized
        }.value
        return image
    }

    // MARK: - Pipeline

    private func process(id: UUID, name: String, source: URL, kind: MapSourceKind,
                         progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        let library = self.library
        let folder = library.directory(for: id)
        do {
            try Task.checkCancellation()
            progress(0.05, kind == .pdf ? "Rendering PDF…" : "Decoding image…")
            let (image, dpi) = try await Task.detached(priority: .userInitiated) {
                try MapImporter.renderBaseImage(from: source)
            }.value
            try Task.checkCancellation()

            let tileSize = TileStore.defaultTileSize
            var document = MapDocument(
                id: id,
                name: name,
                createdAt: Date(),
                lastOpenedAt: nil,
                sourceFileName: source.lastPathComponent,
                sourceKind: kind,
                pixelWidth: image.width,
                pixelHeight: image.height,
                renderDPI: dpi,
                tileSize: tileSize,
                levels: TileStore.levelCount(width: image.width, height: image.height, tileSize: tileSize),
                georef: nil,
                georefMessage: nil,
                pois: [],
                contourRGB: nil
            )
            let tilesURL = library.tilesDirectory(for: document, dark: false)
            let thumbURL = library.thumbnailURL(for: document)

            progress(0.3, "Building tiles…")
            let levels = try await Task.detached(priority: .userInitiated) { () throws -> Int in
                let count = try TileStore.buildPyramid(from: image, into: tilesURL, tileSize: tileSize) { f in
                    progress(0.3 + 0.6 * f, "Building tiles…")
                }
                try MapImporter.writeThumbnail(of: image, to: thumbURL)
                return count
            }.value
            document.levels = levels
            try Task.checkCancellation()

            progress(0.95, "Saving…")
            let saved = document
            await MainActor.run { library.save(saved) }
            progress(1, "Imported")
            return ImportedMap(document: saved, baseImage: image)
        } catch {
            try? FileManager.default.removeItem(at: folder)
            if error is CancellationError { throw error }
            if error is ImportError { throw error }
            throw ImportError.unreadable(error.localizedDescription)
        }
    }

    // MARK: - Rendering (pure, any thread)

    /// Base raster of a stored or incoming source: PDFs are rendered (first page, 300 dpi within
    /// the size limits), images decoded upright (EXIF orientation applied, long side ≤ 6000 px).
    /// Returns the raster and its dpi (PDFs only).
    static func renderBaseImage(from url: URL) throws -> (CGImage, Double?) {
        switch try sourceKind(of: url) {
        case .pdf:
            let (image, dpi) = try renderPDF(at: url)
            return (image, dpi)
        case .image:
            return (try decodeImage(at: url), nil)
        }
    }

    static func renderPDF(at url: URL) throws -> (CGImage, Double) {
        guard let document = CGPDFDocument(url as CFURL) else { throw ImportError.unreadable("not a valid PDF") }
        if document.isEncrypted, !document.isUnlocked {
            _ = document.unlockWithPassword("")
        }
        guard document.isUnlocked else { throw ImportError.unreadable("the PDF is password protected") }
        guard let page = document.page(at: 1) else { throw ImportError.unreadable("the PDF has no pages") }
        let box = page.getBoxRect(.cropBox)
        guard box.width > 0, box.height > 0 else { throw ImportError.unreadable("the PDF page is empty") }
        let plan = pdfRenderPlan(box: box, rotation: Int(page.rotationAngle))
        guard let ctx = TileStore.makeContext(width: plan.width, height: plan.height) else {
            throw ImportError.unreadable("the PDF page is too large to render")
        }
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: plan.width, height: plan.height))
        ctx.interpolationQuality = .high
        ctx.setShouldAntialias(true)
        ctx.concatenate(plan.transform)
        ctx.clip(to: box)
        ctx.drawPDFPage(page)
        guard let image = ctx.makeImage() else { throw ImportError.unreadable("rendering failed") }
        return (image, plan.dpi)
    }

    /// Scale (pixels per point) for a page of `size` points: 300 dpi, long side ≤ 6000 px,
    /// ≤ 36 MP, but long side ≥ 2048 px.
    static func pdfScale(for size: CGSize) -> CGFloat {
        let long = max(size.width, size.height)
        let area = size.width * size.height
        guard long > 0, area > 0 else { return pdfDPI / 72 }
        var scale = pdfDPI / 72
        scale = min(scale, maxLongSide / long)
        scale = min(scale, (maxPixels / area).squareRoot())
        scale = max(scale, minLongSide / long)
        return scale
    }

    /// `rotation` is the page's /Rotate (degrees clockwise, multiple of 90).
    static func pdfRenderPlan(box: CGRect, rotation: Int) -> PDFRenderPlan {
        let r = ((rotation % 360) + 360) % 360
        let quarterTurn = r == 90 || r == 270
        let shownWidth = quarterTurn ? box.height : box.width
        let shownHeight = quarterTurn ? box.width : box.height
        let scale = pdfScale(for: CGSize(width: shownWidth, height: shownHeight))
        let width = max(1, Int((shownWidth * scale).rounded()))
        let height = max(1, Int((shownHeight * scale).rounded()))

        // Applied to page points in this order: move the box to the origin, apply /Rotate
        // (clockwise on screen), scale to pixels, pin the top edge to the bitmap's top row.
        var t = CGAffineTransform(translationX: 0, y: CGFloat(height) - shownHeight * scale)
        t = t.scaledBy(x: scale, y: scale)
        switch r {
        case 90: // (x, y) → (y, W − x)
            t = CGAffineTransform(a: 0, b: -1, c: 1, d: 0, tx: 0, ty: box.width).concatenating(t)
        case 180: // (x, y) → (W − x, H − y)
            t = CGAffineTransform(a: -1, b: 0, c: 0, d: -1, tx: box.width, ty: box.height).concatenating(t)
        case 270: // (x, y) → (H − y, x)
            t = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: box.height, ty: 0).concatenating(t)
        default:
            break
        }
        t = CGAffineTransform(translationX: -box.minX, y: -box.minY).concatenating(t)
        return PDFRenderPlan(width: width, height: height, scale: scale, transform: t)
    }

    /// Decode an image file upright (EXIF orientation applied), long side ≤ 6000 px, into an
    /// opaque sRGB raster (transparent areas become white).
    static func decodeImage(at url: URL) throws -> CGImage {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil), CGImageSourceGetCount(source) > 0 else {
            throw ImportError.unreadable("not a supported image")
        }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: Int(maxLongSide),
            kCGImageSourceShouldCacheImmediately: true,
        ]
        guard let decoded = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            throw ImportError.unreadable("the image could not be decoded")
        }
        guard let ctx = TileStore.makeContext(width: decoded.width, height: decoded.height) else {
            throw ImportError.unreadable("the image is too large")
        }
        let rect = CGRect(x: 0, y: 0, width: decoded.width, height: decoded.height)
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(rect)
        ctx.interpolationQuality = .none
        ctx.draw(decoded, in: rect)
        guard let image = ctx.makeImage() else { throw ImportError.unreadable("the image could not be decoded") }
        return image
    }

    static func writeThumbnail(of image: CGImage, to url: URL) throws {
        let long = max(image.width, image.height)
        let s = min(1, Double(thumbnailLongSide) / Double(max(1, long)))
        let w = max(1, Int((Double(image.width) * s).rounded()))
        let h = max(1, Int((Double(image.height) * s).rounded()))
        guard let ctx = TileStore.makeContext(width: w, height: h) else { throw TileStoreError.contextFailed }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        guard let thumb = ctx.makeImage(), let data = TileStore.jpegData(thumb, quality: 0.8) else {
            throw TileStoreError.encodeFailed("thumbnail")
        }
        try data.write(to: url, options: .atomic)
    }

    // MARK: - Files

    static func sourceKind(of url: URL) throws -> MapSourceKind {
        if let type = UTType(filenameExtension: url.pathExtension.lowercased()) {
            if type.conforms(to: .pdf) { return .pdf }
            if type.conforms(to: .image) { return .image }
        }
        if let data = try? Data(contentsOf: url, options: .mappedIfSafe) {
            if data.starts(with: Array("%PDF-".utf8)) { return .pdf }
            if let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetType(source) != nil {
                return .image
            }
        }
        throw ImportError.unsupported(url.lastPathComponent)
    }

    static func fileExtension(for url: URL, kind: MapSourceKind) -> String {
        if kind == .pdf { return "pdf" }
        let ext = url.pathExtension.lowercased()
        if !ext.isEmpty { return ext }
        if let source = CGImageSourceCreateWithURL(url as CFURL, nil), let type = CGImageSourceGetType(source),
           let preferred = UTType(type as String)?.preferredFilenameExtension {
            return preferred
        }
        return "img"
    }

    static func displayName(for url: URL) -> String {
        let base = url.deletingPathExtension().lastPathComponent.trimmingCharacters(in: .whitespacesAndNewlines)
        return base.isEmpty ? "Untitled map" : base
    }

    /// Copy through NSFileCoordinator so iCloud Drive files are downloaded first.
    private static func coordinatedCopy(from source: URL, to destination: URL) throws {
        var coordinationError: NSError?
        var copyError: Error?
        NSFileCoordinator().coordinate(readingItemAt: source, options: [.withoutChanges], error: &coordinationError) { readURL in
            do {
                try FileManager.default.copyItem(at: readURL, to: destination)
            } catch {
                copyError = error
            }
        }
        if let error = coordinationError ?? copyError {
            throw ImportError.unreadable(error.localizedDescription)
        }
    }

    /// Files opened with "Open in MapMe" land in Documents/Inbox; we keep our own copy.
    private static func removeInboxCopy(_ url: URL) {
        let inbox = URL.documentsDirectory.appendingPathComponent("Inbox", isDirectory: true).standardizedFileURL.path
        if url.standardizedFileURL.path.hasPrefix(inbox) {
            try? FileManager.default.removeItem(at: url)
        }
    }
}
