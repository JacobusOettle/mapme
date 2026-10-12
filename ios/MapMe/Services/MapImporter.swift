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

/// How an incoming image is treated.
enum ImageImportMode {
    /// The image is the map as it is (screenshots, exports, tests).
    case plain
    /// Look for a photographed sheet of paper. When one is found in perspective, `pickCorners`
    /// (the corner editor) confirms its corners before the page is flattened; nil cancels the
    /// import. `fallbackCorners` are offered instead when nothing is detected (UI tests).
    case detectPhoto(PhotoCornerPicker, fallbackCorners: PhotoQuad? = nil)
    /// A page from the document camera: already flattened, georeferenced as a photo.
    case scannedPage
}

/// What the corner editor needs to show.
struct PhotoCornerRequest {
    var name: String
    /// The upright photo; corners are in its pixels.
    var image: CGImage
    /// Downscaled copy for display (long side ≤ `MapImporter.previewLongSide`).
    var preview: CGImage
    var initial: PhotoQuad
    /// The page outline found automatically, if any (the editor's "Auto").
    var detected: PhotoQuad?
    /// Re-adjusting an existing map rather than importing a new one.
    var isAdjusting: Bool
}

/// Presents the corner editor and returns the confirmed corners, or nil when the user cancels.
typealias PhotoCornerPicker = (PhotoCornerRequest) async -> PhotoQuad?

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

/// Turns a PDF/image into a stored MapDocument + tile pyramid + thumbnail (then renders the dark
/// tiles and dark thumbnail in the background: `DarkPrerender`).
final class MapImporter {
    let library: MapLibrary

    static let pdfDPI: CGFloat = 300
    static let maxLongSide: CGFloat = 6000
    static let maxPixels: CGFloat = 36_000_000
    static let minLongSide: CGFloat = 2048
    static let thumbnailLongSide = 640
    /// Corner-editor preview size.
    static let previewLongSide = 2400
    /// Base raster of a flattened photo (the original photo is kept as "source-photo.<ext>").
    static let flattenedFileName = "source.jpg"
    static let photoFileStem = "source-photo"
    static let flattenedJPEGQuality = 0.92

    init(library: MapLibrary) { self.library = library }

    /// `progress(fraction 0…1, step description)` may be called from any thread. `mode` applies to images.
    func importFile(at url: URL, mode: ImageImportMode = .plain,
                    progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
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
        return try await process(id: id, name: Self.displayName(for: url), source: destination, kind: kind, mode: mode,
                                 progress: progress)
    }

    func importImageData(_ data: Data, suggestedName: String, mode: ImageImportMode = .plain,
                         progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
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
                                 kind: .image, mode: mode, progress: progress)
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

    /// The base raster and how it relates to the stored files.
    private struct PreparedBase {
        var image: CGImage
        var dpi: Double?
        var kind: MapSourceKind
        var sourceFileName: String
        var photo: PhotoSource?
    }

    private func process(id: UUID, name: String, source: URL, kind: MapSourceKind, mode: ImageImportMode,
                         progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        let library = self.library
        let folder = library.directory(for: id)
        do {
            try Task.checkCancellation()
            let base: PreparedBase
            if kind == .pdf {
                progress(0.05, "Rendering PDF…")
                let (image, dpi) = try await Task.detached(priority: .userInitiated) {
                    try MapImporter.renderBaseImage(from: source)
                }.value
                base = PreparedBase(image: image, dpi: dpi, kind: .pdf, sourceFileName: source.lastPathComponent, photo: nil)
            } else {
                base = try await prepareImage(name: name, source: source, folder: folder, mode: mode, progress: progress)
            }
            try Task.checkCancellation()
            let image = base.image

            let tileSize = TileStore.defaultTileSize
            var document = MapDocument(
                id: id,
                name: name,
                createdAt: Date(),
                lastOpenedAt: nil,
                sourceFileName: base.sourceFileName,
                sourceKind: base.kind,
                pixelWidth: image.width,
                pixelHeight: image.height,
                renderDPI: base.dpi,
                tileSize: tileSize,
                levels: TileStore.levelCount(width: image.width, height: image.height, tileSize: tileSize),
                georef: nil,
                georefMessage: nil,
                pois: [],
                contourRGB: nil,
                photoSource: base.photo
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
            // Dark tiles and thumbnail in the background, so the first dark view is instant.
            DarkPrerender.start(saved, library: library)
            progress(1, "Imported")
            return ImportedMap(document: saved, baseImage: image)
        } catch {
            try? FileManager.default.removeItem(at: folder)
            if error is CancellationError { throw error }
            if error is ImportError { throw error }
            throw ImportError.unreadable(error.localizedDescription)
        }
    }

    // MARK: - Photos of paper maps

    /// Decode an image and, for photos, find the sheet of paper, let the user confirm its corners
    /// and flatten it. The original photo stays in the map folder for "Adjust Corners".
    private func prepareImage(name: String, source: URL, folder: URL, mode: ImageImportMode,
                              progress: @escaping (Double, String) -> Void) async throws -> PreparedBase {
        progress(0.05, "Decoding image…")
        let decoded = try await Task.detached(priority: .userInitiated) {
            try MapImporter.decodeImage(at: source)
        }.value
        let fileName = source.lastPathComponent
        let plain = PreparedBase(image: decoded, dpi: nil, kind: .image, sourceFileName: fileName, photo: nil)
        let pickCorners: PhotoCornerPicker
        let fallbackCorners: PhotoQuad?
        switch mode {
        case .plain:
            return plain
        case .scannedPage:
            return PreparedBase(image: decoded, dpi: nil, kind: .photo, sourceFileName: fileName, photo: nil)
        case .detectPhoto(let picker, let fallback):
            pickCorners = picker
            fallbackCorners = fallback
        }

        try Task.checkCancellation()
        progress(0.08, "Looking for the edges of the map…")
        let info = PhotoCaptureInfo.read(from: source)
        let size = CGSize(width: decoded.width, height: decoded.height)
        let detected = await PhotoQuadDetector.detect(in: decoded)
        try Task.checkCancellation()

        let initial: PhotoQuad
        switch PhotoGeometry.triage(detected, imageSize: size) {
        case .needsCorners(let quad):
            initial = quad
        case .asIs:
            if let fallback = fallbackCorners, detected == nil {
                initial = fallback
            } else if info.isCamera {
                // A camera photo that fills the frame: use it as is; its corners can be adjusted later.
                let photoName = Self.photoFileName(pathExtension: source.pathExtension)
                try FileManager.default.moveItem(at: source, to: folder.appendingPathComponent(photoName))
                let photo = PhotoSource(fileName: photoName, pixelWidth: decoded.width, pixelHeight: decoded.height,
                                        quad: PhotoQuad.full(size).json, focalLength35mm: info.focalLength35mm)
                return PreparedBase(image: decoded, dpi: nil, kind: .photo, sourceFileName: photoName, photo: photo)
            } else {
                return plain
            }
        }

        let preview = await Task.detached(priority: .userInitiated) {
            MapPhotoRectifier.downscaled(decoded, maxLongSide: MapImporter.previewLongSide)
        }.value ?? decoded
        let request = PhotoCornerRequest(name: name, image: decoded, preview: preview, initial: initial,
                                         detected: detected, isAdjusting: false)
        guard let corners = await pickCorners(request) else { throw CancellationError() }
        try Task.checkCancellation()

        progress(0.12, "Flattening the map…")
        let photoName = Self.photoFileName(pathExtension: source.pathExtension)
        try FileManager.default.moveItem(at: source, to: folder.appendingPathComponent(photoName))
        let focal = info.focalLength35mm
        let flat = try await Task.detached(priority: .userInitiated) { () throws -> CGImage? in
            try MapImporter.flattened(decoded, quad: corners, focalLength35mm: focal)
        }.value
        let photo = PhotoSource(fileName: photoName, pixelWidth: decoded.width, pixelHeight: decoded.height,
                                quad: corners.json, focalLength35mm: focal)
        guard let flat else {
            // The corners are the photo's own corners: the photo is the base raster.
            return PreparedBase(image: decoded, dpi: nil, kind: .photo, sourceFileName: photoName, photo: photo)
        }
        let flatURL = folder.appendingPathComponent(Self.flattenedFileName)
        try await Task.detached(priority: .userInitiated) {
            try MapImporter.writeFlattened(flat, to: flatURL)
        }.value
        return PreparedBase(image: flat, dpi: nil, kind: .photo, sourceFileName: Self.flattenedFileName, photo: photo)
    }

    /// The original photo of a photo map, decoded upright, with its stored corners (in the decoded
    /// photo's pixels) as the editor's starting point.
    func loadPhoto(for map: MapDocument) async throws -> PhotoCornerRequest {
        guard let source = map.photoSource, let stored = PhotoQuad(json: source.quad) else {
            throw ImportError.unreadable("the original photo of this map is not available")
        }
        let url = library.directory(for: map.id).appendingPathComponent(source.fileName)
        let (image, preview) = try await Task.detached(priority: .userInitiated) { () throws -> (CGImage, CGImage) in
            let image = try MapImporter.decodeImage(at: url)
            return (image, MapPhotoRectifier.downscaled(image, maxLongSide: MapImporter.previewLongSide) ?? image)
        }.value
        let sx = CGFloat(image.width) / CGFloat(max(1, source.pixelWidth))
        let sy = CGFloat(image.height) / CGFloat(max(1, source.pixelHeight))
        return PhotoCornerRequest(name: map.name, image: image, preview: preview, initial: stored.scaled(x: sx, y: sy),
                                  detected: nil, isAdjusting: true)
    }

    /// Flatten a photo map again with new corners: new base raster, tiles and thumbnail (built
    /// aside and swapped in at the end, so a failure leaves the map as it was). The georeference is
    /// carried over through the change of flattening; locate again to refine it.
    func reflatten(_ map: MapDocument, photo: CGImage, corners: PhotoQuad,
                   progress: @escaping (Double, String) -> Void) async throws -> ImportedMap {
        guard let source = map.photoSource else { throw ImportError.unreadable("the original photo of this map is not available") }
        let library = self.library
        let folder = library.directory(for: map.id)
        let photoSize = CGSize(width: photo.width, height: photo.height)
        let oldCorners = PhotoQuad(json: source.quad)?.scaled(x: photoSize.width / CGFloat(max(1, source.pixelWidth)),
                                                               y: photoSize.height / CGFloat(max(1, source.pixelHeight)))
        progress(0.08, "Flattening the map…")
        let focal = source.focalLength35mm
        let flat = try await Task.detached(priority: .userInitiated) { () throws -> CGImage? in
            try MapImporter.flattened(photo, quad: corners, focalLength35mm: focal)
        }.value
        try Task.checkCancellation()
        let image = flat ?? photo

        var document = map
        document.sourceFileName = flat == nil ? source.fileName : Self.flattenedFileName
        document.pixelWidth = image.width
        document.pixelHeight = image.height
        document.photoSource = PhotoSource(fileName: source.fileName, pixelWidth: photo.width, pixelHeight: photo.height,
                                           quad: corners.json, focalLength35mm: focal)
        if let oldCorners {
            document.georef = Self.carriedGeoreference(map.georef, oldCorners: oldCorners, oldWidth: map.pixelWidth,
                                                       oldHeight: map.pixelHeight, newCorners: corners,
                                                       newWidth: image.width, newHeight: image.height)
        } else {
            document.georef = nil
        }

        let tileSize = document.tileSize
        let tilesURL = library.tilesDirectory(for: document, dark: false)
        let stagingURL = folder.appendingPathComponent("tiles-staging", isDirectory: true)
        let thumbURL = library.thumbnailURL(for: document)
        let flatURL = folder.appendingPathComponent(Self.flattenedFileName)
        // Everything drawn from the old page: dark tiles (current and legacy), profile, dark
        // thumbnail, and the text read off it (in the old page's pixels).
        let staleDarkFiles = [library.tilesDirectory(for: document, dark: true), library.legacyDarkTilesDirectory(for: document),
                              library.darkProfileURL(for: document), library.darkThumbnailURL(for: document),
                              library.ocrCacheURL(for: document)]
        // A dark render of the old page must not write into the new page's folders.
        await DarkPrerender.cancelAndWait(map.id)
        progress(0.3, "Building tiles…")
        let levels = try await Task.detached(priority: .userInitiated) { () throws -> Int in
            let fm = FileManager.default
            defer { try? fm.removeItem(at: stagingURL) }
            let count = try TileStore.buildPyramid(from: image, into: stagingURL, tileSize: tileSize) { f in
                progress(0.3 + 0.6 * f, "Building tiles…")
            }
            try Task.checkCancellation()
            if let flat {
                try MapImporter.writeFlattened(flat, to: flatURL)
            } else {
                try? fm.removeItem(at: flatURL)
            }
            try? fm.removeItem(at: tilesURL)
            for url in staleDarkFiles {
                try? fm.removeItem(at: url)
            }
            try fm.moveItem(at: stagingURL, to: tilesURL)
            try MapImporter.writeThumbnail(of: image, to: thumbURL)
            return count
        }.value
        document.levels = levels

        progress(0.95, "Saving…")
        let saved = document
        await MainActor.run { library.save(saved) }
        DarkPrerender.start(saved, library: library)
        progress(1, "Flattened")
        return ImportedMap(document: saved, baseImage: image)
    }

    /// The page flattened with Core Image, or nil when `quad` is the whole photo (nothing to do).
    static func flattened(_ photo: CGImage, quad: PhotoQuad, focalLength35mm: Double?) throws -> CGImage? {
        let size = CGSize(width: photo.width, height: photo.height)
        if PhotoGeometry.isFullImage(quad, imageSize: size) { return nil }
        return try MapPhotoRectifier.flatten(photo, quad: quad, focalLength35mm: focalLength35mm,
                                             maxLongSide: Double(maxLongSide))
    }

    static func writeFlattened(_ image: CGImage, to url: URL) throws {
        guard let data = TileStore.jpegData(image, quality: flattenedJPEGQuality) else {
            throw TileStoreError.encodeFailed("flattened photo")
        }
        try data.write(to: url, options: .atomic)
    }

    static func photoFileName(pathExtension: String) -> String {
        let clean = pathExtension.lowercased().filter { $0.isLetter || $0.isNumber }
        return clean.isEmpty ? photoFileStem : photoFileStem + "." + clean
    }

    /// A georeference of the page flattened from `oldCorners` (into `oldWidth` × `oldHeight`)
    /// re-expressed for the same photo flattened from `newCorners` (into `newWidth` × `newHeight`):
    /// new pixel → photo pixel → old pixel → Mercator. Corners are in the same photo's pixels.
    /// A warp (folded paper) comes along: a lattice of the same size over the new image whose node
    /// offsets are the old warp at the matching old pixels, so `H_new = H_old ∘ newToOld` and
    /// `B_new(p) ≈ B_old(newToOld(p))`.
    static func carriedGeoreference(_ georef: Georeference?, oldCorners: PhotoQuad, oldWidth: Int, oldHeight: Int,
                                    newCorners: PhotoQuad, newWidth: Int, newHeight: Int) -> Georeference? {
        guard let georef,
              let oldRect = PhotoGeometry.rectification(quad: oldCorners, width: oldWidth, height: oldHeight),
              let newRect = PhotoGeometry.rectification(quad: newCorners, width: newWidth, height: newHeight),
              let newToPhoto = newRect.inverse
        else { return nil }
        let newToOld = PhotoGeometry.compose(oldRect, newToPhoto)
        let h = PhotoGeometry.compose(georef.pixelToMercator, newToOld)
        guard h.m.allSatisfy({ $0.isFinite }) else { return nil }
        var carried = georef
        carried.pixelToMercator = h
        carried.warp = georef.warp.flatMap { old -> MeshWarp? in
            let warp = old.resampled(cols: old.cols, rows: old.rows, width: Double(newWidth), height: Double(newHeight)) { p in
                newToOld.apply(p)
            }
            return warp.isValid ? warp : nil
        }
        return carried
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
        case .image, .photo:
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
