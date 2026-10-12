import SwiftUI
import UIKit

/// Where the user is relative to a map's footprint.
enum MapProximity: Equatable {
    case unknown
    case onMap
    /// Distance and initial bearing (degrees) to the nearest point of the map.
    case away(meters: Double, bearing: Double)

    static func of(_ map: MapDocument, from here: LatLon?) -> MapProximity {
        guard let here, let georef = map.georef, map.pixelWidth > 0, map.pixelHeight > 0 else { return .unknown }
        let px = georef.pixel(for: here)
        guard px.x.isFinite, px.y.isFinite else { return .unknown }
        let w = CGFloat(map.pixelWidth)
        let h = CGFloat(map.pixelHeight)
        if px.x >= 0, px.y >= 0, px.x <= w, px.y <= h { return .onMap }
        let edge = CGPoint(x: min(max(px.x, 0), w), y: min(max(px.y, 0), h))
        let target = georef.latLon(forPixel: edge)
        return .away(meters: Geodesy.distance(here, target), bearing: Geodesy.bearing(here, target))
    }

    var isOnMap: Bool { self == .onMap }
}

enum MapStatus {
    /// "Located · grid ±2 m", "Located · contours ±25 m", "Manual", "Not located".
    static func text(for map: MapDocument) -> String {
        guard let g = map.georef else { return "Not located" }
        var accuracy = ""
        if let rms = g.rmsMeters, rms.isFinite {
            accuracy = " ±\(max(1, Int(rms.rounded()))) m"
        }
        switch g.method {
        case .grid: return "Located · grid" + accuracy
        case .labels: return "Located · labels" + accuracy
        case .contours: return "Located · contours" + accuracy
        case .manual: return "Manual"
        }
    }

    static func symbol(for map: MapDocument) -> String {
        guard let g = map.georef else { return "questionmark.circle.fill" }
        switch g.method {
        case .grid, .labels: return "number.circle.fill"
        case .contours: return "mountain.2.circle.fill"
        case .manual: return "hand.tap.fill"
        }
    }

    static func color(for map: MapDocument) -> Color {
        guard let g = map.georef else { return Theme.warn }
        return g.method == .manual ? Theme.accent : Theme.good
    }
}

/// Decoded thumbnails, loaded off the main thread.
final class MapThumbnailCache {
    static let shared = MapThumbnailCache()
    private let cache = NSCache<NSURL, UIImage>()

    func cached(_ url: URL) -> UIImage? { cache.object(forKey: url as NSURL) }

    /// Forget a thumbnail whose file was rewritten (a photo map flattened again).
    func invalidate(_ url: URL) { cache.removeObject(forKey: url as NSURL) }

    /// Loads the thumbnail at `url`. When the file does not exist yet, `make` (if given) runs first,
    /// off the main thread, to create it (dark thumbnails of maps imported before dark maps).
    func load(_ url: URL, make: (() -> Void)? = nil) async -> UIImage? {
        if let hit = cached(url) { return hit }
        let image = await Task.detached(priority: .utility) { () -> UIImage? in
            if let make, !FileManager.default.fileExists(atPath: url.path) {
                make()
            }
            guard let data = try? Data(contentsOf: url), let image = UIImage(data: data) else { return nil }
            return image.preparingForDisplay() ?? image
        }.value
        if let image { cache.setObject(image, forKey: url as NSURL) }
        return image
    }
}

struct MapCardView: View {
    let map: MapDocument
    /// `thumb.jpg`, or `thumb-dark.jpg` when maps are shown dark.
    let thumbnailURL: URL
    let proximity: MapProximity
    let units: DistanceUnits
    /// Creates the thumbnail file when it is missing (the dark one of an older map). Runs off the
    /// main thread.
    var makeThumbnail: (() -> Void)? = nil

    @State private var thumbnail: UIImage?

    private static let thumbHeight: CGFloat = 170

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ZStack(alignment: .topLeading) {
                Color.clear
                    .frame(height: Self.thumbHeight)
                    .frame(maxWidth: .infinity)
                    .overlay(alignment: .top) { thumbnailImage }
                    .animation(.easeOut(duration: 0.25), value: thumbnail == nil)
                    .clipped()
                LinearGradient(colors: [Theme.surface.opacity(0), Theme.surface.opacity(0.95)],
                               startPoint: .center, endPoint: .bottom)
                MapStatusChip(map: map)
                    .padding(12)
                if map.isPhoto {
                    Image(systemName: "camera.fill")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)
                        .frame(width: 28, height: 28)
                        .background(.ultraThinMaterial, in: Circle())
                        .overlay(Circle().stroke(Theme.stroke, lineWidth: 1))
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .accessibilityLabel("Photo of a paper map")
                }
            }
            .frame(height: Self.thumbHeight)

            VStack(alignment: .leading, spacing: 6) {
                Text(map.name)
                    .font(.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                proximityRow
            }
            .padding(.horizontal, 14)
            .padding(.top, 8)
            .padding(.bottom, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .stroke(proximity.isOnMap ? Theme.userRed.opacity(0.55) : Theme.stroke, lineWidth: 1)
        )
        .shadow(color: proximity.isOnMap ? Theme.userRed.opacity(0.3) : Color.clear, radius: 14, y: 4)
        .contentShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .task(id: thumbnailKey) {
            thumbnail = await MapThumbnailCache.shared.load(thumbnailURL, make: makeThumbnail)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(map.name), \(MapStatus.text(for: map)), \(proximityText ?? "")")
        .accessibilityIdentifier("mapCard")
    }

    @ViewBuilder
    private var thumbnailImage: some View {
        if let thumbnail {
            Image(uiImage: thumbnail)
                .resizable()
                .scaledToFill()
                .frame(maxWidth: .infinity, maxHeight: Self.thumbHeight, alignment: .top)
                .transition(.opacity)
        } else {
            ZStack {
                Theme.surfaceRaised
                Image(systemName: "map")
                    .font(.system(size: 34, weight: .light))
                    .foregroundStyle(Theme.textSecondary)
            }
        }
    }

    /// Changes when the thumbnail file is rewritten (corners adjusted), so it is loaded again.
    private var thumbnailKey: String {
        var corners = ""
        if let quad = map.photoSource?.quad {
            let values: [Double] = quad.flatMap { $0 }
            corners = values.map { String(format: "%.0f", $0) }.joined(separator: ",")
        }
        return "\(thumbnailURL.path)|\(map.pixelWidth)x\(map.pixelHeight)|\(corners)"
    }

    private var proximityText: String? {
        switch proximity {
        case .onMap: return "You're on this map"
        case .away(let meters, let bearing):
            return "\(GeoFormat.distance(meters, units: units)) \(Geodesy.compassPoint(bearing))"
        case .unknown: return nil
        }
    }

    @ViewBuilder
    private var proximityRow: some View {
        HStack(spacing: 8) {
            switch proximity {
            case .onMap:
                LibraryPulsingDot()
                Text("You're on this map")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.userRed)
            case .away(_, let bearing):
                Image(systemName: "location.north.fill")
                    .font(.caption)
                    .rotationEffect(.degrees(bearing))
                    .foregroundStyle(Theme.accent)
                Text(proximityText ?? "")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textSecondary)
                    .monospacedDigit()
            case .unknown:
                Image(systemName: "clock")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                Text("Imported \(map.createdAt.formatted(.relative(presentation: .named)))")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
    }
}

struct MapStatusChip: View {
    let map: MapDocument

    var body: some View {
        let color = MapStatus.color(for: map)
        HStack(spacing: 5) {
            Image(systemName: MapStatus.symbol(for: map))
                .font(.caption2.weight(.bold))
            Text(MapStatus.text(for: map))
                .font(.caption.weight(.semibold))
                .monospacedDigit()
        }
        .foregroundStyle(color)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(.ultraThinMaterial, in: Capsule())
        .background(color.opacity(0.12), in: Capsule())
        .overlay(Capsule().stroke(color.opacity(0.35), lineWidth: 1))
    }
}

/// Small red "you are here" dot with an expanding ring.
struct LibraryPulsingDot: View {
    var size: CGFloat = 10
    @State private var animating = false

    var body: some View {
        ZStack {
            Circle()
                .fill(Theme.userRed.opacity(0.5))
                .frame(width: size, height: size)
                .scaleEffect(animating ? 2.8 : 1)
                .opacity(animating ? 0 : 1)
            Circle()
                .fill(Theme.userRed)
                .frame(width: size, height: size)
                .overlay(Circle().stroke(Color.white, lineWidth: 1.5))
        }
        .frame(width: size * 1.6, height: size * 1.6)
        .onAppear {
            animating = false
            withAnimation(.easeOut(duration: 1.5).repeatForever(autoreverses: false)) { animating = true }
        }
        .accessibilityHidden(true)
    }
}

/// Springy press feedback for cards.
struct LibraryCardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .brightness(configuration.isPressed ? -0.04 : 0)
            .animation(.spring(response: 0.28, dampingFraction: 0.7), value: configuration.isPressed)
    }
}
