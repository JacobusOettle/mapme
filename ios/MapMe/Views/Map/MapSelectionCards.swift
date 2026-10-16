import SwiftUI

/// The card of a selected geo item: each kind's card lives in its feature's files.
@MainActor
struct SelectedGeoItemCard: View {
    let ref: GeoItemRef
    let context: MapScreenContext
    let map: MapDocument

    var body: some View {
        switch ref.kind {
        case .waypoint:
            WaypointCard(id: ref.id, context: context, map: map)    // routes
        case .route:
            RouteCard(id: ref.id, context: context, map: map)       // routes
        case .note:
            NoteCard(id: ref.id, context: context, map: map)        // notes
        }
    }
}

/// The long-press pin's card: where it is, and what to make of it. Each action is its feature's.
@MainActor
struct DroppedPinCard: View {
    let pin: DroppedPin
    let context: MapScreenContext
    let map: MapDocument
    @Environment(AppModel.self) private var model

    var body: some View {
        let units = model.settings.units
        let here = model.location.location.map { LatLon($0.coordinate) }
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                Image(systemName: "mappin.circle.fill")
                    .font(.system(size: 34))
                    .foregroundStyle(Color.white, Theme.userRed)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Dropped Pin")
                        .font(.headline)
                    HStack(spacing: 6) {
                        if let here {
                            let d = Geodesy.distance(here, pin.position)
                            Text("\(GeoFormat.distance(d, units: units)) \(Geodesy.compassPoint(Geodesy.bearing(here, pin.position)))")
                                .monospacedDigit()
                            Text("·")
                        }
                        Text(GeoFormat.coordinate(pin.position, format: model.settings.coordinateFormat))
                            .monospacedDigit()
                            .lineLimit(1)
                            .minimumScaleFactor(0.7)
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Button {
                    withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) { context.droppedPin = nil }
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(Color.primary.opacity(0.1), in: Circle())
                }
                .buttonStyle(PressScaleButtonStyle())
                .accessibilityLabel("Remove pin")
            }
            HStack(spacing: 8) {
                pinAction("Waypoint", symbol: "flag.fill", id: "dropPinWaypointButton") {
                    WaypointActions.saveWaypoint(at: pin.position, context: context, model: model)
                }
                pinAction("Note", symbol: "note.text", id: "dropPinNoteButton") {
                    NoteActions.addNote(at: pin.position, context: context, model: model)
                }
                pinAction("Photo", symbol: "camera.fill", id: "dropPinPhotoButton") {
                    NoteActions.addPhoto(at: pin.position, context: context, model: model)
                }
                pinAction("Route", symbol: "point.topleft.down.to.point.bottomright.curvepath", id: "dropPinRouteButton") {
                    RouteActions.routeHere(to: pin.position, context: context, model: model)
                }
            }
        }
        .padding(14)
        .floatingChrome(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("droppedPinCard")
    }

    private func pinAction(_ title: String, symbol: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 4) {
                Image(systemName: symbol)
                    .font(.system(size: 16, weight: .semibold))
                Text(title)
                    .font(.caption.weight(.semibold))
            }
            .foregroundStyle(Theme.accent)
            .frame(maxWidth: .infinity, minHeight: 52)
            .background(Color.primary.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(PressScaleButtonStyle(scale: 0.95))
        .accessibilityIdentifier(id)
    }
}
