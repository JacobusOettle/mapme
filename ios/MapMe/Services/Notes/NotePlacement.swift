import Foundation

/// Where a new note was started (docs/design/explorer.md, Notes and photos).
enum NoteOrigin: Equatable, Sendable {
    /// The side column's note button: a note where you are.
    case here
    /// The dropped pin's Note or Photo: a note at a long-pressed point.
    case pin(LatLon)
}

/// One place a note can be pinned, as the editor offers it.
struct NotePlaceCandidate: Hashable, Identifiable, Sendable {
    enum Kind: String, Hashable, Sendable {
        /// Where an existing note already is.
        case current
        /// The dropped pin.
        case pin
        /// Where the photo was taken: its own GPS position (EXIF), or your fix when you took it.
        case photo
        /// Your position now.
        case me
        /// The middle of the map view.
        case mapCentre
    }

    var kind: Kind
    var position: LatLon
    /// How a photo placed here got its position.
    var placement: PhotoPlacement

    var id: Kind { kind }

    var title: String {
        switch kind {
        case .current: return "Where it is now"
        case .pin: return "At the dropped pin"
        case .photo: return placement == .exif ? "Where the photo was taken" : "Where you took the photo"
        case .me: return "At my location"
        case .mapCentre: return "At the map's centre"
        }
    }
}

/// The places a note can go and the one chosen (docs/design/explorer.md, Notes and photos):
///
/// * from the dropped pin, the pin; a photo's own position wins only when the user picks it, and
///   the editor asks when it is far ("This photo was taken 2.1 km from the pin. Use its
///   location?");
/// * from the note button, a photo's own position (EXIF, or your fix when you took it with the
///   camera); a photo without one asks: at my location (default when there is a fix) or at the
///   map's centre;
/// * a text note goes where you are, else at the map's centre.
struct NotePlacementChoice: Equatable, Sendable {
    var candidates: [NotePlaceCandidate]
    var selected: NotePlaceCandidate.Kind
    /// A question or explanation under the choices.
    var message: String?

    /// A photo taken at least this far from the pin makes the editor ask which to use.
    static let farPhotoMeters = 50.0
    /// "At my location" is not offered for an existing note closer than this to you.
    static let moveMinimumMeters = 5.0

    /// Nil when no place is known (no fix, no map): the note cannot be saved yet.
    var selectedCandidate: NotePlaceCandidate? {
        candidates.first { $0.kind == selected } ?? candidates.first
    }

    /// For a new note.
    /// - Parameters:
    ///   - photoPosition: where the photo was taken (EXIF), or your fix when the camera took it.
    ///   - photoPlacement: `.exif` or `.myLocation` (camera), for `photoPosition`.
    ///   - hasPhoto: the note has a photo (with or without a position).
    static func forNew(origin: NoteOrigin, hasPhoto: Bool, photoPosition: LatLon?, photoPlacement: PhotoPlacement = .exif,
                       fix: LatLon?, mapCentre: LatLon?, units: DistanceUnits) -> NotePlacementChoice {
        var candidates: [NotePlaceCandidate] = []
        var selected: NotePlaceCandidate.Kind
        var message: String?
        let photo = photoPosition.map { NotePlaceCandidate(kind: .photo, position: $0, placement: photoPlacement) }
        // The camera's position is your fix: "at my location" would offer the same place twice.
        let me = (photo?.placement == .myLocation ? nil : fix).map {
            NotePlaceCandidate(kind: .me, position: $0, placement: .myLocation)
        }
        let centre = mapCentre.map { NotePlaceCandidate(kind: .mapCentre, position: $0, placement: .manual) }
        switch origin {
        case .pin(let pin):
            candidates.append(NotePlaceCandidate(kind: .pin, position: pin, placement: .manual))
            candidates += [photo, me].compactMap { $0 }
            selected = .pin
            if let photo {
                let d = Geodesy.distance(pin, photo.position)
                if d >= farPhotoMeters {
                    let distance = GeoFormat.distance(d, units: units)
                    message = photo.placement == .exif
                        ? "This photo was taken \(distance) from the pin. Use its location?"
                        : "You took this photo \(distance) from the pin. Pin it where you took it?"
                }
            }
        case .here:
            candidates = [photo, me, centre].compactMap { $0 }
            if photo != nil {
                selected = .photo
            } else if me != nil {
                selected = .me
            } else {
                selected = .mapCentre
            }
            if hasPhoto, photo == nil {
                message = "This photo has no location. Choose where to pin it."
            }
        }
        if candidates.isEmpty {
            message = "No position yet: wait for a GPS fix, or long-press the map to place the note."
        }
        return NotePlacementChoice(candidates: candidates, selected: selected, message: message)
    }

    /// For an existing note: where it is, or moved to you.
    static func forExisting(_ note: MapNote, fix: LatLon?) -> NotePlacementChoice {
        var candidates = [NotePlaceCandidate(kind: .current, position: note.position, placement: note.photo?.placement ?? .manual)]
        if let fix, Geodesy.distance(fix, note.position) >= moveMinimumMeters {
            candidates.append(NotePlaceCandidate(kind: .me, position: fix, placement: .myLocation))
        }
        return NotePlacementChoice(candidates: candidates, selected: .current, message: nil)
    }
}
