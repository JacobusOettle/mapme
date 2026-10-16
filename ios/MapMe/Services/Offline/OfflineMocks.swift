import Foundation
import UIKit

// Stand-ins for UI tests of offline terrain (Debug builds; docs/design/explorer.md, Tests):
//
// * `-mockOfflineDownloads`: regions and trailheads download from a fake tile server (a small
//   valid PNG per tile after a short delay, sizes for HEAD requests) into their own folders
//   (Application Support/Terrain-mock, OfflineRegions-mock), emptied at every launch: no real
//   download in CI, and nothing a mock writes ever reaches the real terrain the matcher reads.
// * `-mockRegionPlace "Name=s,w,n,e"`: the region picker's place search answers this place
//   instead of Apple Maps.

enum OfflineMocks {
    static let downloadsArgument = "-mockOfflineDownloads"
    static let placeArgument = "-mockRegionPlace"

    /// Fake downloads are on for this run.
    static var mocksDownloads: Bool {
        #if DEBUG
        return ProcessInfo.processInfo.arguments.contains(downloadsArgument)
        #else
        return false
        #endif
    }

    /// The place `-mockRegionPlace "Name=s,w,n,e"` stands in for, if any.
    static var place: (name: String, bounds: GeoBounds)? {
        #if DEBUG
        guard let value = AppSettings.launchValue(placeArgument, in: ProcessInfo.processInfo.arguments) else { return nil }
        return parsePlace(value)
        #else
        return nil
        #endif
    }

    /// "Name=s,w,n,e" → the name and its box.
    static func parsePlace(_ value: String) -> (name: String, bounds: GeoBounds)? {
        let parts = value.split(separator: "=", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { return nil }
        let numbers = parts[1].split(separator: ",").compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
        guard numbers.count == 4, numbers[0] < numbers[2] else { return nil }
        let name = parts[0].trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return nil }
        return (name, GeoBounds(south: numbers[0], west: numbers[1], north: numbers[2], east: numbers[3]))
    }

    /// A session whose every request is answered by `MockTileURLProtocol`.
    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockTileURLProtocol.self]
        config.httpMaximumConnectionsPerHost = 8
        return URLSession(configuration: config)
    }
}

/// A fake Terrarium server: GET answers a valid 256 × 256 PNG (flat terrain at 0 m) after
/// `delay`; HEAD answers a size between 40 and 60 KB that depends on the tile, as real tiles vary.
final class MockTileURLProtocol: URLProtocol {
    /// Seconds before a GET is answered (slow enough for a progress screenshot).
    static let delay: TimeInterval = 0.06

    static let png: Data = {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 256, height: 256), format: format)
        let image = renderer.image { ctx in
            // Terrarium: (R × 256 + G + B / 256) − 32768 = 0 m.
            UIColor(red: 128 / 255, green: 0, blue: 0, alpha: 1).setFill()
            ctx.fill(CGRect(x: 0, y: 0, width: 256, height: 256))
        }
        return image.pngData() ?? Data()
    }()

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        let isHead = request.httpMethod == "HEAD"
        let size = 40_000 + abs(url.path.hashValue % 20_000)
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + (isHead ? 0.01 : Self.delay)) { [weak self] in
            guard let self else { return }
            let body = isHead ? Data() : Self.png
            let length = isHead ? size : body.count
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                           headerFields: ["Content-Length": "\(length)", "Content-Type": "image/png"])
            if let response {
                self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            }
            if !body.isEmpty {
                self.client?.urlProtocol(self, didLoad: body)
            }
            self.client?.urlProtocolDidFinishLoading(self)
        }
    }

    override func stopLoading() {}
}
