import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// What a photo's metadata says about where and when it was taken (EXIF and GPS, as ImageIO reads
/// them from the JPEG and HEIC files of cameras and the Photos library).
struct PhotoMetadata: Equatable, Sendable {
    /// GPSLatitude/GPSLongitude with their N/S and E/W references.
    var position: LatLon?
    /// DateTimeOriginal in its own time zone (OffsetTimeOriginal), else the GPS time stamp (UTC),
    /// else DateTimeOriginal read in the device's time zone.
    var takenAt: Date?
    /// GPSImgDirection: where the camera pointed, degrees clockwise from north.
    var headingDegrees: Double?

    /// Reads ImageIO's property dictionary (`CGImageSourceCopyPropertiesAtIndex`).
    static func read(_ properties: [String: Any]) -> PhotoMetadata {
        let gps = properties[kCGImagePropertyGPSDictionary as String] as? [String: Any] ?? [:]
        let exif = properties[kCGImagePropertyExifDictionary as String] as? [String: Any] ?? [:]
        let tiff = properties[kCGImagePropertyTIFFDictionary as String] as? [String: Any] ?? [:]
        return PhotoMetadata(position: position(gps: gps), takenAt: date(exif: exif, gps: gps, tiff: tiff),
                             headingDegrees: heading(gps: gps))
    }

    /// The GPS position, or nil when there is none or it is not a place on Earth. (0, 0) is
    /// treated as "no fix": some cameras write it when they had none.
    static func position(gps: [String: Any]) -> LatLon? {
        guard var lat = number(gps[kCGImagePropertyGPSLatitude as String]),
              var lon = number(gps[kCGImagePropertyGPSLongitude as String]),
              lat.isFinite, lon.isFinite
        else { return nil }
        // Writers store the magnitude with a reference letter; a few store signed values instead.
        if let ref = (gps[kCGImagePropertyGPSLatitudeRef as String] as? String)?.uppercased(), ref.hasPrefix("S") {
            lat = -abs(lat)
        }
        if let ref = (gps[kCGImagePropertyGPSLongitudeRef as String] as? String)?.uppercased(), ref.hasPrefix("W") {
            lon = -abs(lon)
        }
        guard abs(lat) <= 90, abs(lon) <= 180, abs(lat) > 1e-7 || abs(lon) > 1e-7 else { return nil }
        return LatLon(lat: lat, lon: lon)
    }

    /// GPSImgDirection in 0..<360 (true or magnetic north as the camera wrote it: a few degrees
    /// apart, which a note's photo does not need to resolve).
    static func heading(gps: [String: Any]) -> Double? {
        guard let value = number(gps[kCGImagePropertyGPSImgDirection as String]), value.isFinite else { return nil }
        let wrapped = value.truncatingRemainder(dividingBy: 360)
        return wrapped < 0 ? wrapped + 360 : wrapped
    }

    static func date(exif: [String: Any], gps: [String: Any], tiff: [String: Any]) -> Date? {
        let original = (exif[kCGImagePropertyExifDateTimeOriginal as String] as? String)
            ?? (exif[kCGImagePropertyExifDateTimeDigitized as String] as? String)
            ?? (tiff[kCGImagePropertyTIFFDateTime as String] as? String)
        if let original, let offset = exif[kCGImagePropertyExifOffsetTimeOriginal as String] as? String,
           let zone = timeZone(offset: offset), let date = parse(original, in: zone) {
            return date
        }
        if let day = gps[kCGImagePropertyGPSDateStamp as String] as? String,
           let time = gps[kCGImagePropertyGPSTimeStamp as String] as? String,
           let utc = TimeZone(secondsFromGMT: 0),
           // "13:30:00.00": seconds are enough.
           let date = parse("\(day) \(time.prefix(8))", in: utc) {
            return date
        }
        if let original { return parse(original, in: .current) }
        return nil
    }

    /// "+02:00", "-05:30" (EXIF 2.31 OffsetTime*).
    static func timeZone(offset: String) -> TimeZone? {
        let s = offset.trimmingCharacters(in: .whitespaces)
        guard s.count == 6, let sign = s.first, sign == "+" || sign == "-" else { return nil }
        let parts = s.dropFirst().split(separator: ":")
        guard parts.count == 2, let h = Int(parts[0]), let m = Int(parts[1]), h <= 14, m < 60 else { return nil }
        let seconds = (h * 3600 + m * 60) * (sign == "-" ? -1 : 1)
        return TimeZone(secondsFromGMT: seconds)
    }

    /// EXIF's "yyyy:MM:dd HH:mm:ss" in `zone`.
    private static func parse(_ text: String, in zone: TimeZone) -> Date? {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = zone
        formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
        return formatter.date(from: text.trimmingCharacters(in: .whitespaces))
    }

    private static func number(_ value: Any?) -> Double? {
        guard let value else { return nil }
        switch value {
        case let n as NSNumber: return n.doubleValue
        case let s as String: return Double(s.trimmingCharacters(in: .whitespaces))
        default: return nil
        }
    }
}

/// Turns a photo from the camera or the library into a note's photo (docs/design/explorer.md,
/// Notes and photos): decoded and re-encoded (≤ 2048 px, JPEG 0.8, upright) without any metadata,
/// plus a 160 px square thumbnail for the map. The original is not kept; what its metadata said
/// (position, time, direction) is returned for the note to use. Blocking and thread-safe: run it
/// off the main thread.
enum NotePhotoCodec {
    /// Longest side of a stored photo, pixels.
    static let maxPixelSize = 2048
    /// Side of the square thumbnail, pixels (a 34 pt pin at 3× is 102 px).
    static let thumbnailPixelSize = 160
    static let jpegQuality = 0.8

    /// Re-encodes `data` (JPEG, HEIC, PNG…) into `directory` as "<id>.jpg" and "<id>-thumb.jpg".
    /// - Parameter placement: how the photo is placed when its metadata has no position (a photo
    ///   with GPS is `.exif`).
    static func prepare(_ data: Data, directory: URL, placement: PhotoPlacement = .manual) throws -> PreparedPhoto {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else {
            throw NoteError.unreadablePhoto
        }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [String: Any] ?? [:]
        let metadata = PhotoMetadata.read(properties)
        // The thumbnail API decodes straight to the size wanted (a 48 MP HEIC never sits in memory
        // whole) and applies the EXIF orientation, so the stored photo is upright without it.
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        guard let decoded = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            throw NoteError.unreadablePhoto
        }
        let image = opaque(decoded)
        guard let jpeg = encodeJPEG(image),
              let square = squareThumbnail(image, side: thumbnailPixelSize),
              let thumb = encodeJPEG(square)
        else { throw NoteError.unreadablePhoto }

        let id = UUID()
        let photo = NotePhoto(id: id, fileName: "\(id.uuidString).jpg", pixelWidth: image.width, pixelHeight: image.height,
                              takenAt: metadata.takenAt, headingDegrees: metadata.headingDegrees,
                              placement: metadata.position != nil ? .exif : placement)
        let photoURL = directory.appendingPathComponent(photo.fileName)
        let thumbURL = directory.appendingPathComponent(photo.thumbnailFileName)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try jpeg.write(to: photoURL, options: .atomic)
            try thumb.write(to: thumbURL, options: .atomic)
        } catch {
            try? FileManager.default.removeItem(at: photoURL)
            try? FileManager.default.removeItem(at: thumbURL)
            throw NoteError.couldNotSave
        }
        return PreparedPhoto(photo: photo, exifPosition: metadata.position)
    }

    /// A JPEG of `image` with no metadata at all (no EXIF, no GPS: only what ImageIO needs to
    /// describe the pixels).
    static func encodeJPEG(_ image: CGImage, quality: Double = jpegQuality) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil)
        else { return nil }
        let properties: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }

    /// The middle square of `image`, `side` pixels a side.
    static func squareThumbnail(_ image: CGImage, side: Int) -> CGImage? {
        let w = image.width, h = image.height
        guard w > 0, h > 0, side > 0 else { return nil }
        let s = min(w, h)
        guard let square = image.cropping(to: CGRect(x: (w - s) / 2, y: (h - s) / 2, width: s, height: s)),
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(square, in: CGRect(x: 0, y: 0, width: side, height: side))
        return ctx.makeImage()
    }

    /// `image` without transparency (a PNG screenshot): JPEG has none, and ImageIO would turn
    /// transparent pixels black.
    static func opaque(_ image: CGImage) -> CGImage {
        switch image.alphaInfo {
        case .none, .noneSkipFirst, .noneSkipLast:
            return image
        default:
            break
        }
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return image }
        let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(rect)
        ctx.draw(image, in: rect)
        return ctx.makeImage() ?? image
    }

    /// Decodes an image file (nil when it is missing or unreadable), downsized to `maxPixelSize`
    /// when given.
    static func loadImage(at url: URL, maxPixelSize: Int? = nil) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil), CGImageSourceGetCount(source) > 0 else { return nil }
        if let maxPixelSize {
            let options: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
                kCGImageSourceShouldCacheImmediately: true,
            ]
            return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        }
        let options: [CFString: Any] = [kCGImageSourceShouldCacheImmediately: true]
        return CGImageSourceCreateImageAtIndex(source, 0, options as CFDictionary)
    }
}
