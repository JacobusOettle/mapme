import Foundation

/// Where a profile's elevations came from (the profile says so, docs/design/explorer.md, Profile).
enum ProfileSource: Equatable, Sendable {
    /// The terrain model (`mm_terrain_profile`, Terrarium DEM).
    case terrain
    /// The elevations in a GPX file (`<ele>`).
    case file
    /// Your phone's GPS altitude (a recorded track).
    case gps
    /// No elevations: the distance alone.
    case distanceOnly

    /// "Elevation from the terrain model", for the chart's footnote.
    var caption: String {
        switch self {
        case .terrain: return "Elevation from the terrain model"
        case .file: return "Elevation from the GPX file"
        case .gps: return "Elevation from GPS altitude"
        case .distanceOnly: return "Elevation needs terrain (offline)"
        }
    }
}

/// A profile and where its elevations came from.
struct RouteProfile: Equatable, Sendable {
    var profile: ElevationProfile
    var source: ProfileSource

    var hasElevation: Bool { source != .distanceOnly && profile.samples.contains { $0.elevationM != nil } }
}

/// Route statistics and elevation profiles from the terrain (`mm_terrain_profile` through
/// `TerrainProvider`): distance, ascent, descent, walking time (docs/design/explorer.md, Routes).
enum RoutePlanner {
    /// Profile samples asked of the core for a route (evenly spaced).
    static let routeSamples = 1000
    /// …and for the live trail (redrawn often).
    static let trailSamples = 300
    /// Smoothing of measured elevations (GPX, GPS) before climbs are counted, metres (Gaussian σ):
    /// GPS altitude wanders by ±5–10 m from fix to fix.
    static let measuredSmoothingM = 30.0

    /// Stats and profile of `points` (a route, an imported track, the recorded trail). Downloads
    /// the terrain it lacks when online; offline, answers from what is on the device (distance
    /// alone when there is no terrain). Nil without points. Run off the main thread.
    static func profile(of points: [LatLon], terrain: TerrainProvider, maxZoom: Int = 13,
                        maxSamples: Int = RoutePlanner.routeSamples) async -> ElevationProfile? {
        guard !points.isEmpty else { return nil }
        var request = ProfileRequest(points: points, maxZoom: maxZoom)
        request.maxSamples = maxSamples
        let query = request
        if let answer = await terrain.resolve({ store in store.profile(query) }), let value = answer.value,
           value.distanceM.isFinite {
            return value
        }
        // No store, or the engine could not answer: the distance is still known.
        return distanceOnlyProfile(points)
    }

    /// The best profile of a route: the terrain's, else the file's own elevations, else the
    /// distance alone.
    static func bestProfile(for route: Route, terrain: TerrainProvider) async -> RouteProfile? {
        let points = route.latLons
        guard let fromTerrain = await profile(of: points, terrain: terrain) else { return nil }
        if fromTerrain.ascentM != nil, fromTerrain.samples.contains(where: { $0.elevationM != nil }) {
            return RouteProfile(profile: fromTerrain, source: .terrain)
        }
        let elevations = route.points.map(\.ele)
        if elevations.compactMap({ $0 }).count >= 2, let measured = measuredProfile(points, elevations: elevations) {
            return RouteProfile(profile: measured, source: .file)
        }
        return RouteProfile(profile: fromTerrain, source: .distanceOnly)
    }

    /// The profile of a recorded walk: the terrain's when there is terrain (GPS altitude wanders),
    /// GPS altitude otherwise.
    static func trackProfile(of points: [TrackPoint], terrain: TerrainProvider) async -> RouteProfile? {
        guard points.count >= 2 else { return nil }
        let line = points.map(\.latLon)
        if let fromTerrain = await profile(of: line, terrain: terrain, maxSamples: trailSamples),
           fromTerrain.ascentM != nil, fromTerrain.samples.contains(where: { $0.elevationM != nil }) {
            return RouteProfile(profile: fromTerrain, source: .terrain)
        }
        if let measured = measuredProfile(line, elevations: points.map(\.altitude), maxSamples: trailSamples) {
            return RouteProfile(profile: measured, source: .gps)
        }
        return RouteProfile(profile: distanceOnlyProfile(line), source: .distanceOnly)
    }

    /// The distance alone: no elevations.
    static func distanceOnlyProfile(_ points: [LatLon]) -> ElevationProfile {
        let cumulative = RouteGeometry.cumulativeDistances(points)
        let total = cumulative.last ?? 0
        return ElevationProfile(distanceM: total, ascentM: nil, descentM: nil, minM: nil, maxM: nil,
                                samples: [ProfileSample(distanceM: 0, elevationM: nil),
                                          ProfileSample(distanceM: total, elevationM: nil)],
                                vertexDistanceM: cumulative, vertexElevationM: points.map { _ in nil },
                                zoom: nil, missingFraction: 1)
    }

    /// A profile from measured elevations along the points (a GPX file's `<ele>`, GPS altitude):
    /// resampled evenly, smoothed (Gaussian σ 30 m) and climbs counted with 3 m hysteresis, like
    /// the core's. Nil with fewer than two elevations.
    static func measuredProfile(_ points: [LatLon], elevations: [Double?],
                                maxSamples: Int = RoutePlanner.routeSamples) -> ElevationProfile? {
        guard points.count == elevations.count, points.count >= 2 else { return nil }
        let cumulative = RouteGeometry.cumulativeDistances(points)
        let known = zip(cumulative, elevations).compactMap { d, e -> (Double, Double)? in
            guard let e, e.isFinite else { return nil }
            return (d, e)
        }
        guard known.count >= 2, let total = cumulative.last, total > 0 else { return nil }

        // Even samples, linearly interpolated between the known elevations (held at the ends).
        let count = max(2, min(maxSamples, Int(total / 5) + 1))
        let step = total / Double(count - 1)
        var raw = [Double](repeating: 0, count: count)
        var j = 0
        for i in 0..<count {
            let d = Double(i) * step
            while j < known.count - 2, known[j + 1].0 < d { j += 1 }
            let (d0, e0) = known[j], (d1, e1) = known[j + 1]
            if d <= d0 {
                raw[i] = e0
            } else if d >= d1 {
                raw[i] = e1
            } else {
                raw[i] = e0 + (e1 - e0) * (d - d0) / (d1 - d0)
            }
        }

        // Gaussian smoothing along the line.
        let sigma = max(measuredSmoothingM, step)
        let reach = Int((3 * sigma / step).rounded(.up))
        var smooth = raw
        if reach > 0 {
            var weights: [Double] = []
            for k in 0...reach {
                let x = Double(k) * step / sigma
                weights.append(exp(-0.5 * x * x))
            }
            for i in 0..<count {
                var sum = raw[i] * weights[0], wsum = weights[0]
                for k in 1...reach {
                    if i - k >= 0 {
                        sum += raw[i - k] * weights[k]
                        wsum += weights[k]
                    }
                    if i + k < count {
                        sum += raw[i + k] * weights[k]
                        wsum += weights[k]
                    }
                }
                smooth[i] = sum / wsum
            }
        }

        let climbs = RouteGeometry.climbs(smooth, hysteresis: Track.ascentHysteresis)
        let samples = smooth.enumerated().map { ProfileSample(distanceM: Double($0.offset) * step, elevationM: $0.element) }
        return ElevationProfile(distanceM: total, ascentM: climbs.ascent, descentM: climbs.descent,
                                minM: smooth.min(), maxM: smooth.max(), samples: samples,
                                vertexDistanceM: cumulative, vertexElevationM: elevations,
                                zoom: nil, missingFraction: 0)
    }

    /// `RouteStats` for a route from its profile.
    static func stats(for route: Route, profile: ElevationProfile?) -> RouteStats {
        RouteStats(distanceM: profile?.distanceM ?? 0, ascentM: profile?.ascentM, descentM: profile?.descentM,
                   minElevationM: profile?.minM, maxElevationM: profile?.maxM, demZoom: profile?.zoom,
                   geometryHash: route.geometryHash)
    }

    /// Climb and descent after `fromDistanceM` along a profile (3 m hysteresis): what is left of
    /// a followed route. Nil without elevations.
    static func remainingClimbs(of profile: ElevationProfile, fromDistanceM: Double) -> (ascent: Double, descent: Double)? {
        var elevations: [Double] = []
        for s in profile.samples where s.distanceM >= fromDistanceM {
            if let e = s.elevationM { elevations.append(e) }
        }
        guard !elevations.isEmpty else { return nil }
        return RouteGeometry.climbs(elevations, hysteresis: Track.ascentHysteresis)
    }

    /// Elevation at `distanceM` along a profile (linear between samples).
    static func elevation(in profile: ElevationProfile, atDistance distanceM: Double) -> Double? {
        let s = profile.samples
        guard let first = s.first else { return nil }
        if distanceM <= first.distanceM { return first.elevationM }
        for i in 1..<s.count where s[i].distanceM >= distanceM {
            guard let e0 = s[i - 1].elevationM, let e1 = s[i].elevationM else { return s[i].elevationM ?? s[i - 1].elevationM }
            let span = s[i].distanceM - s[i - 1].distanceM
            return span > 0 ? e0 + (e1 - e0) * (distanceM - s[i - 1].distanceM) / span : e1
        }
        return s.last?.elevationM
    }
}
