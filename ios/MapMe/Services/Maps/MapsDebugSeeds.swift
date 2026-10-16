#if DEBUG
import CoreGraphics
import Foundation

/// Debug-only fixtures for the maps feature's UI tests (docs/design/explorer.md §10).
@MainActor
enum MapsDebugSeeds {
    static let sampleName = "Grootkloof sample"
    static let overviewName = "Grootkloof overview"
    /// The overview is the sample at ¼ resolution: pyramid level 2 becomes its level 0.
    static let overviewLevel = 2

    /// `-seedSecondSampleMap`: once the sample is imported and located, clones it as
    /// "Grootkloof overview" at ¼ resolution (its georeference `H · diag(4, 4, 1)`), so two maps
    /// cover the same ground at different scales. With `open`, shows the overview afterwards.
    /// Waits up to 10 minutes for the sample (a UI test may be importing it meanwhile).
    static func seedSecondSampleMap(_ model: AppModel, open: Bool) {
        Task { @MainActor in
            for _ in 0..<600 {
                if let existing = model.library.maps.first(where: { $0.name == overviewName }) {
                    if open { openWhenIdle(model, id: existing.id) }
                    return
                }
                if let sample = model.library.maps.first(where: { $0.name == sampleName && $0.isGeoreferenced }),
                   model.importJob == nil {
                    if let made = await clone(sample, library: model.library), open {
                        openWhenIdle(model, id: made.id)
                    }
                    return
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    /// `-openWorldView`: shows the world view once the launch (and any sample import) has settled.
    static func openWorldViewWhenReady(_ model: AppModel) {
        Task { @MainActor in
            for _ in 0..<600 {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                if model.importJob == nil, model.pendingPhoto == nil {
                    model.openWorld()
                    return
                }
            }
        }
    }

    private static func openWhenIdle(_ model: AppModel, id: UUID) {
        Task { @MainActor in
            for _ in 0..<600 {
                if model.importJob == nil, model.pendingPhoto == nil {
                    model.openMap(id)
                    return
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    /// Copies `sample`'s pyramid from level 2 down into a new map folder and saves the overview.
    static func clone(_ sample: MapDocument, library: MapLibrary) async -> MapDocument? {
        guard let georef = sample.georef, sample.levels > 1 else { return nil }
        let k = min(overviewLevel, sample.levels - 1)
        let f = Double(1 << k)
        let size = TileStore.levelSize(width: sample.pixelWidth, height: sample.pixelHeight, level: k)
        let id = UUID()
        let source = library.directory(for: sample.id)
        let target = library.directory(for: id)
        let copied = await Task.detached(priority: .userInitiated) { () -> Bool in
            copyPyramid(from: source, to: target, fromLevel: k, levels: sample.levels,
                        extraFiles: ["thumb.jpg", "thumb-dark.jpg", "dark-profile.json", sample.sourceFileName])
        }.value
        guard copied else { return nil }

        // Overview pixel p is sample pixel f·p: H' = H · diag(f, f, 1); the warp's lattice scales too.
        var h = georef.pixelToMercator.m
        for row in 0..<3 {
            h[row * 3] *= f
            h[row * 3 + 1] *= f
        }
        var warp = georef.warp
        if let w = warp {
            warp = MeshWarp(cols: w.cols, rows: w.rows, origin: w.origin / f, spacing: w.spacing / f, offsets: w.offsets)
        }
        let scaled = Georeference(pixelToMercator: Homography(h), method: georef.method, confidence: georef.confidence,
                                  rmsMeters: georef.rmsMeters, createdAt: georef.createdAt, warp: warp)
        var doc = MapDocument(id: id, name: overviewName, createdAt: Date(), lastOpenedAt: nil,
                              sourceFileName: sample.sourceFileName, sourceKind: sample.sourceKind,
                              pixelWidth: size.width, pixelHeight: size.height,
                              renderDPI: sample.renderDPI.map { $0 / f }, tileSize: sample.tileSize,
                              levels: sample.levels - k, georef: scaled, georefMessage: sample.georefMessage,
                              pois: [], contourRGB: sample.contourRGB)
        doc.pois = sample.pois.map { poi in
            var copy = poi
            copy.id = UUID()
            return copy
        }
        library.save(doc)
        return doc
    }

    /// Copies `tiles/` and `tiles-dark-v2/` levels `fromLevel…` to levels `0…`, and the listed
    /// files. Blocking.
    nonisolated static func copyPyramid(from source: URL, to target: URL, fromLevel k: Int, levels: Int,
                                        extraFiles: [String]) -> Bool {
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: target, withIntermediateDirectories: true)
            for folder in ["tiles", "tiles-dark-v2"] {
                let from = source.appendingPathComponent(folder, isDirectory: true)
                guard fm.fileExists(atPath: from.path) else { continue }
                let to = target.appendingPathComponent(folder, isDirectory: true)
                try fm.createDirectory(at: to, withIntermediateDirectories: true)
                for level in k..<max(k, levels) {
                    let src = from.appendingPathComponent("\(level)", isDirectory: true)
                    guard fm.fileExists(atPath: src.path) else { continue }
                    try fm.copyItem(at: src, to: to.appendingPathComponent("\(level - k)", isDirectory: true))
                }
            }
            for name in extraFiles {
                let src = source.appendingPathComponent(name)
                guard fm.fileExists(atPath: src.path) else { continue }
                try? fm.copyItem(at: src, to: target.appendingPathComponent(name))
            }
            return true
        } catch {
            try? fm.removeItem(at: target)
            return false
        }
    }
}
#endif
