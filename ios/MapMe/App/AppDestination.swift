import Foundation
import UniformTypeIdentifiers

/// Screens pushed on the library's navigation stack.
enum AppDestination: Hashable {
    /// A map (`MapScreen`). Auto-switching may then show another map in the same screen.
    case map(UUID)
    /// All your maps on Earth, with your routes, waypoints, notes and terrain (`WorldView`).
    case world
}

extension UTType {
    /// GPS Exchange Format (declared in ios/project.yml, UTImportedTypeDeclarations).
    static let gpx = UTType(importedAs: "com.topografix.gpx", conformingTo: .xml)
}

/// What an incoming file is (Files, "Open in", AirDrop, share sheet).
enum IncomingFile {
    case gpx
    /// A PDF or image: a map to import.
    case map

    static func kind(of url: URL) -> IncomingFile {
        if url.pathExtension.lowercased() == "gpx" { return .gpx }
        if let type = UTType(filenameExtension: url.pathExtension), type.conforms(to: .gpx) { return .gpx }
        return .map
    }
}
