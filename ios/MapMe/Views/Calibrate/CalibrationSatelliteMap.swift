import CoreLocation
import MapKit
import SwiftUI

/// Apple satellite (hybrid, 3D terrain) map where the user taps the real-world spot
/// matching a point on their map. Numbered pins mirror the pairs on the map pane.
struct CalibrationSatelliteMap: View {
    @Binding var camera: MapCameraPosition
    let pairs: [CalibrationPair]
    let pendingCoordinate: LatLon?
    /// Where the current georeference expects the pending map point to be.
    let predicted: LatLon?
    var onTap: (CLLocationCoordinate2D) -> Void
    /// Visible region and camera distance (metres) after the user moves the map.
    var onCameraChange: (MKCoordinateRegion, Double) -> Void

    private struct NumberedPair: Identifiable {
        let index: Int
        let pair: CalibrationPair
        var id: UUID { pair.id }
    }

    var body: some View {
        let numbered = pairs.enumerated().map { NumberedPair(index: $0.offset, pair: $0.element) }
        MapReader { proxy in
            Map(position: $camera, interactionModes: [.pan, .zoom, .rotate]) {
                UserAnnotation()
                ForEach(numbered) { item in
                    Annotation("", coordinate: item.pair.coordinate.coordinate, anchor: .center) {
                        CalibrationPin(number: item.index + 1, color: CalibrationPalette.color(item.index),
                                       gps: item.pair.fromGPS)
                    }
                }
                if let predicted {
                    Annotation("", coordinate: predicted.coordinate, anchor: .center) {
                        CalibrationPredictedRing()
                    }
                }
                if let pendingCoordinate {
                    Annotation("", coordinate: pendingCoordinate.coordinate, anchor: .center) {
                        CalibrationPendingPin(number: pairs.count + 1)
                    }
                }
            }
            .mapStyle(.hybrid(elevation: .realistic))
            .mapControls {
                MapUserLocationButton()
                MapCompass()
            }
            .onTapGesture { point in
                if let coordinate = proxy.convert(point, from: .local) {
                    onTap(coordinate)
                }
            }
            .onMapCameraChange(frequency: .onEnd) { context in
                onCameraChange(context.region, context.camera.distance)
            }
        }
    }
}

/// Numbered control point: a ring with a dot on the exact spot and a number badge.
struct CalibrationPin: View {
    let number: Int
    let color: Color
    var gps: Bool = false

    var body: some View {
        ZStack {
            Circle()
                .stroke(Color.black.opacity(0.45), lineWidth: 5)
                .frame(width: 22, height: 22)
            Circle()
                .stroke(color, lineWidth: 2.5)
                .frame(width: 22, height: 22)
            Circle()
                .fill(color)
                .frame(width: 5, height: 5)
            ZStack {
                Circle()
                    .fill(color)
                Circle()
                    .stroke(Color.white, lineWidth: 1.5)
                if gps {
                    Image(systemName: "location.fill")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Color.black)
                } else {
                    Text("\(number)")
                        .font(.system(size: 10, weight: .heavy, design: .rounded))
                        .foregroundStyle(Color.black)
                }
            }
            .frame(width: 17, height: 17)
            .offset(x: 13, y: -13)
        }
        .frame(width: 48, height: 48)
        .shadow(color: Color.black.opacity(0.5), radius: 2, y: 1)
        .accessibilityLabel("Point \(number)")
    }
}

/// A spot tapped on the satellite map waiting for its partner on the user's map.
struct CalibrationPendingPin: View {
    let number: Int
    @State private var pulse = false

    init(number: Int) {
        self.number = number
    }

    var body: some View {
        ZStack {
            Circle()
                .stroke(Theme.accent.opacity(0.6), lineWidth: 2)
                .frame(width: 22, height: 22)
                .scaleEffect(pulse ? 2.2 : 1)
                .opacity(pulse ? 0 : 1)
            Circle()
                .stroke(Theme.accent, lineWidth: 2.5)
                .frame(width: 22, height: 22)
            Circle()
                .fill(Theme.accent)
                .frame(width: 5, height: 5)
            Text("\(number)?")
                .font(.system(size: 10, weight: .heavy, design: .rounded))
                .foregroundStyle(Color.black)
                .padding(.horizontal, 4)
                .frame(minWidth: 17, minHeight: 17)
                .background(Theme.accent, in: Capsule())
                .offset(x: 15, y: -13)
        }
        .frame(width: 56, height: 56)
        .shadow(color: Color.black.opacity(0.5), radius: 2, y: 1)
        .onAppear {
            withAnimation(.easeOut(duration: 1.3).repeatForever(autoreverses: false)) { pulse = true }
        }
        .accessibilityLabel("Pending point \(number)")
    }
}

/// Where the current fit says the tapped map point lies: a hint, not a pin.
struct CalibrationPredictedRing: View {
    @State private var spin = false

    init() {}

    var body: some View {
        ZStack {
            Circle()
                .stroke(Theme.accent.opacity(0.9), style: StrokeStyle(lineWidth: 2, dash: [5, 4]))
                .frame(width: 46, height: 46)
                .rotationEffect(.degrees(spin ? 360 : 0))
            Image(systemName: "plus")
                .font(.system(size: 12, weight: .bold))
                .foregroundStyle(Theme.accent)
        }
        .frame(width: 56, height: 56)
        .shadow(color: Color.black.opacity(0.6), radius: 2)
        .onAppear {
            withAnimation(.linear(duration: 6).repeatForever(autoreverses: false)) { spin = true }
        }
        .allowsHitTesting(false)
        .accessibilityLabel("Predicted position")
    }
}
