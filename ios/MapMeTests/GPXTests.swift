import Foundation
import XCTest
@testable import MapMe

/// GPX import and export (docs/design/explorer.md, GPX): files as Gaia GPS, AllTrails and an old
/// Garmin/GPSBabel GPX 1.0 write them, round trips of everything MapMe exports, and files that
/// cannot be read.
final class GPXTests: XCTestCase {
    /// As Gaia GPS exports a folder: metadata, waypoints with `sym`, CDATA and extensions, a planned
    /// route and a recorded track in two segments.
    static let gaia = """
    <?xml version="1.0" encoding="UTF-8"?>
    <gpx xmlns="http://www.topografix.com/GPX/1/1" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" \
    version="1.1" creator="GaiaGPS" \
    xsi:schemaLocation="http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd">
      <metadata>
        <name>Bannerman Pass</name>
        <link href="https://www.gaiagps.com/"><text>Gaia GPS</text></link>
        <time>2026-04-02T05:12:44Z</time>
      </metadata>
      <wpt lat="-29.105600" lon="29.452400">
        <ele>2802.0</ele>
        <time>2026-04-01T10:00:00Z</time>
        <name>Bannerman Hut</name>
        <desc><![CDATA[Stone hut, 6 bunks. Water 200 m east & <clean>.]]></desc>
        <sym>shelter</sym>
        <extensions>
          <gaia:icon xmlns:gaia="http://www.gaiagps.com/xmlschemas/v1">shelter-24.png</gaia:icon>
          <gaia:name xmlns:gaia="http://www.gaiagps.com/xmlschemas/v1">Not the name</gaia:name>
        </extensions>
      </wpt>
      <wpt lat="-29.0921" lon="29.4433"><ele>2420</ele><time>2026-04-01T09:00:00.250Z</time><name>Stream crossing</name><sym>water</sym></wpt>
      <wpt lat="-29.0803" lon="29.4310"><ele>1980</ele><name>Injisuthi trailhead</name><sym>trailhead</sym></wpt>
      <rte>
        <name>Bannerman Pass (planned)</name>
        <rtept lat="-29.0803" lon="29.4310"><name>Trailhead</name></rtept>
        <rtept lat="-29.0921" lon="29.4433"></rtept>
        <rtept lat="-29.1056" lon="29.4524"><name>Hut</name></rtept>
      </rte>
      <trk>
        <name>Bannerman Pass (recorded)</name>
        <desc>Day 1</desc>
        <extensions>
          <gpxx:TrackExtension xmlns:gpxx="http://www.garmin.com/xmlschemas/GpxExtensions/v3">
            <gpxx:DisplayColor>Red</gpxx:DisplayColor>
          </gpxx:TrackExtension>
        </extensions>
        <trkseg>
          <trkpt lat="-29.0803" lon="29.4310"><ele>1980.2</ele><time>2026-04-01T06:00:00Z</time></trkpt>
          <trkpt lat="-29.0850" lon="29.4352"><ele>2101.5</ele><time>2026-04-01T06:40:00Z</time></trkpt>
          <trkpt lat="-29.0921" lon="29.4433"><ele>2420.0</ele><time>2026-04-01T08:55:00Z</time></trkpt>
        </trkseg>
        <trkseg>
          <trkpt lat="-29.0990" lon="29.4480"><ele>2650.8</ele><time>2026-04-01T09:40:00Z</time></trkpt>
          <trkpt lat="-29.1056" lon="29.4524"><ele>2802.0</ele><time>2026-04-01T10:00:00Z</time></trkpt>
        </trkseg>
      </trk>
    </gpx>
    """

    /// As AllTrails exports a trail: a CDATA name, one track, elevations, no times.
    static let allTrails = """
    <?xml version="1.0" encoding="UTF-8"?>
    <gpx creator="AllTrails.com" version="1.1" xmlns="http://www.topografix.com/GPX/1/1" \
    xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" \
    xsi:schemaLocation="http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd">
      <metadata>
        <name><![CDATA[Amphitheatre and Tugela Falls]]></name>
        <desc><![CDATA[]]></desc>
        <link href="https://www.alltrails.com/explore/trail/south-africa/free-state/tugela-falls"><text>AllTrails</text></link>
      </metadata>
      <trk>
        <name><![CDATA[Amphitheatre and Tugela Falls]]></name>
        <src>AllTrails</src>
        <trkseg>
          <trkpt lat="-28.7487" lon="28.8885"><ele>2539.7</ele></trkpt>
          <trkpt lat="-28.7512" lon="28.8921"><ele>2610.2</ele></trkpt>
          <trkpt lat="-28.7540" lon="28.8960"><ele>2702.9</ele></trkpt>
          <trkpt lat="-28.7561" lon="28.9010"><ele>2840.4</ele></trkpt>
          <trkpt lat="-28.7570" lon="28.9071"><ele>2935.0</ele></trkpt>
          <trkpt lat="-28.7577" lon="28.9101"><ele>2957.3</ele></trkpt>
        </trkseg>
      </trk>
    </gpx>
    """

    /// GPX 1.0 as old Garmin units and GPSBabel write it: the name on <gpx>, <cmt> and <desc>.
    static let gpx10 = """
    <?xml version="1.0"?>
    <gpx version="1.0" creator="GPSBabel - https://www.gpsbabel.org" xmlns="http://www.topografix.com/GPX/1/0">
    <name>Old export</name>
    <time>2004-05-01T10:00:00Z</time>
    <wpt lat="-29.100000" lon="29.400000"><ele>2000.000000</ele><name>CAMP1</name><cmt>Camp by the river</cmt><desc>CAMP1</desc><sym>Campground</sym></wpt>
    <wpt lat="-29.110000" lon="29.410000"><name>PK2</name><sym>Summit</sym></wpt>
    <wpt lat="95.0" lon="29.41"><name>Broken</name></wpt>
    <rte><name>Day 2</name><rtept lat="-29.100000" lon="29.400000"><name>A</name></rtept><rtept lat="-29.110000" lon="29.410000"><name>B</name></rtept></rte>
    </gpx>
    """

    private func parse(_ text: String, file: String = "test.gpx") throws -> GPXDocument {
        try GPXService.parse(Data(text.utf8), fileName: file)
    }

    func testGaiaExport() throws {
        let doc = try parse(Self.gaia, file: "Bannerman Pass.gpx")
        XCTAssertEqual(doc.name, "Bannerman Pass")
        XCTAssertEqual(doc.creator, "GaiaGPS")
        XCTAssertEqual(doc.waypoints.map(\.name), ["Bannerman Hut", "Stream crossing", "Injisuthi trailhead"])
        XCTAssertEqual(doc.waypoints.map(\.symbol), [.hut, .water, .start])
        let hut = doc.waypoints[0]
        XCTAssertEqual(hut.position.lat, -29.1056, accuracy: 1e-9)
        XCTAssertEqual(hut.position.lon, 29.4524, accuracy: 1e-9)
        XCTAssertEqual(hut.elevationM, 2802)
        XCTAssertEqual(hut.comment, "Stone hut, 6 bunks. Water 200 m east & <clean>.")
        XCTAssertEqual(hut.createdAt, ISO8601DateFormatter().date(from: "2026-04-01T10:00:00Z"))
        let nine = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-04-01T09:00:00Z"))
        XCTAssertEqual(doc.waypoints[1].createdAt.timeIntervalSince(nine), 0.25, accuracy: 0.001, "fractional seconds")
        XCTAssertEqual(hut.origin, .gpx(fileName: "Bannerman Pass.gpx"))

        XCTAssertEqual(doc.routes.count, 2)
        let planned = doc.routes[0]
        XCTAssertEqual(planned.kind, .gpxRoute)
        XCTAssertEqual(planned.name, "Bannerman Pass (planned)")
        XCTAssertEqual(planned.points.count, 3)
        XCTAssertTrue(planned.points.allSatisfy(\.isAnchor))
        XCTAssertEqual(planned.points.map(\.name), ["Trailhead", nil, "Hut"])
        let recorded = doc.routes[1]
        XCTAssertEqual(recorded.kind, .gpxTrack)
        XCTAssertEqual(recorded.name, "Bannerman Pass (recorded)")
        XCTAssertEqual(recorded.comment, "Day 1")
        XCTAssertEqual(recorded.points.count, 5, "the two segments are joined")
        XCTAssertFalse(recorded.points.contains(where: \.isAnchor), "a track's points are not handles")
        XCTAssertEqual(recorded.points.map(\.ele), [1980.2, 2101.5, 2420.0, 2650.8, 2802.0])
        XCTAssertEqual(recorded.displayColor, .teal)
        XCTAssertEqual(recorded.origin, .gpx(fileName: "Bannerman Pass.gpx"))
    }

    func testAllTrailsExport() throws {
        let doc = try parse(Self.allTrails, file: "tugela-falls.gpx")
        XCTAssertEqual(doc.name, "Amphitheatre and Tugela Falls")
        XCTAssertTrue(doc.waypoints.isEmpty)
        XCTAssertEqual(doc.routes.count, 1)
        let track = try XCTUnwrap(doc.routes.first)
        XCTAssertEqual(track.kind, .gpxTrack)
        XCTAssertEqual(track.name, "Amphitheatre and Tugela Falls")
        XCTAssertEqual(track.points.count, 6)
        XCTAssertEqual(track.points.first?.ele, 2539.7)
        XCTAssertEqual(track.points.last?.ele, 2957.3)
        // Its own elevations give a profile without terrain.
        let profile = try XCTUnwrap(RoutePlanner.measuredProfile(track.latLons, elevations: track.points.map(\.ele)))
        XCTAssertGreaterThan(try XCTUnwrap(profile.ascentM), 300)
        XCTAssertEqual(profile.distanceM, RouteGeometry.length(track.latLons), accuracy: 1)
    }

    func testGPX10() throws {
        let doc = try parse(Self.gpx10, file: "old.gpx")
        XCTAssertEqual(doc.name, "Old export")
        XCTAssertEqual(doc.waypoints.map(\.name), ["CAMP1", "PK2"])
        XCTAssertEqual(doc.waypoints[0].symbol, .camp)
        XCTAssertEqual(doc.waypoints[0].comment, "Camp by the river", "the name repeated in <desc> is not a comment")
        XCTAssertEqual(doc.waypoints[0].elevationM, 2000)
        XCTAssertEqual(doc.waypoints[1].symbol, .peak)
        XCTAssertEqual(doc.skippedPoints, 1, "latitude 95° is left out")
        XCTAssertEqual(doc.routes.map(\.name), ["Day 2"])
        XCTAssertEqual(doc.routes[0].anchors.map(\.name), ["A", "B"])
    }

    func testRoundTripOfEverythingMapMeExports() throws {
        let waypoints = WaypointSymbol.allCases.enumerated().map { i, symbol in
            Waypoint(name: "Point \(i) <\(symbol.label)> & co", position: LatLon(lat: -25.84 + Double(i) * 0.001, lon: 27.39),
                     elevationM: i % 2 == 0 ? 1500 + Double(i) : nil, symbol: symbol,
                     comment: i == 3 ? "Fill bottles\nhere" : nil)
        }
        let planned = Route(name: "Ridge \"loop\"", kind: .planned, points: [
            RoutePoint(lat: -25.8460, lon: 27.3870, isAnchor: true, name: "Camp"),
            RoutePoint(lat: -25.8450, lon: 27.3880, isAnchor: false),
            RoutePoint(lat: -25.8440, lon: 27.3885, isAnchor: false),
            RoutePoint(lat: -25.8405, lon: 27.3920, isAnchor: true),
            RoutePoint(lat: -25.8290, lon: 27.4080, isAnchor: true, name: "Hamerkop Ridge"),
        ])
        var track = Route(name: "Yesterday", kind: .gpxTrack, points: (0..<50).map { i in
            RoutePoint(lat: -25.85 + Double(i) * 1e-4, lon: 27.40, ele: 1400 + Double(i), isAnchor: false)
        })
        track.comment = "Recorded"
        let note = MapNote(position: LatLon(lat: -25.84, lon: 27.40), elevationM: 1490, text: "Rock art\nunder the overhang")

        let text = GPXService.export(name: "Everything", waypoints: waypoints, routes: [planned, track], notes: [note])
        XCTAssertTrue(text.hasPrefix("<?xml"))
        let doc = try parse(text, file: "Everything.gpx")
        XCTAssertEqual(doc.creator, "MapMe")
        XCTAssertEqual(doc.name, "Everything")

        XCTAssertEqual(doc.waypoints.count, waypoints.count + 1)
        for (a, b) in zip(waypoints, doc.waypoints) {
            XCTAssertEqual(b.name, a.name)
            XCTAssertEqual(b.position.lat, a.position.lat, accuracy: 1e-7)
            XCTAssertEqual(b.position.lon, a.position.lon, accuracy: 1e-7)
            XCTAssertEqual(b.symbol, a.symbol, "\(a.symbol)")
            XCTAssertEqual(b.comment, a.comment)
            if let e = a.elevationM {
                XCTAssertEqual(try XCTUnwrap(b.elevationM), e, accuracy: 0.05)
            } else {
                XCTAssertNil(b.elevationM)
            }
            XCTAssertEqual(b.createdAt.timeIntervalSince1970, a.createdAt.timeIntervalSince1970, accuracy: 1)
        }
        // A note comes back as a waypoint with its text.
        let noteBack = try XCTUnwrap(doc.waypoints.last)
        XCTAssertEqual(noteBack.name, "Rock art")
        XCTAssertEqual(noteBack.comment, "Rock art\nunder the overhang")

        XCTAssertEqual(doc.routes.count, 2)
        let route = doc.routes[0]
        XCTAssertEqual(route.name, "Ridge \"loop\"")
        XCTAssertEqual(route.kind, .gpxRoute)
        XCTAssertEqual(route.points.map(\.isAnchor), planned.points.map(\.isAnchor), "traced points stay trace")
        XCTAssertEqual(route.points.map(\.name), planned.points.map(\.name))
        for (a, b) in zip(planned.points, route.points) {
            XCTAssertEqual(a.lat, b.lat, accuracy: 1e-7)
            XCTAssertEqual(a.lon, b.lon, accuracy: 1e-7)
        }
        let back = doc.routes[1]
        XCTAssertEqual(back.kind, .gpxTrack)
        XCTAssertEqual(back.name, "Yesterday")
        XCTAssertEqual(back.comment, "Recorded")
        XCTAssertEqual(back.points.count, 50)
        XCTAssertEqual(try XCTUnwrap(back.points[10].ele), 1410, accuracy: 0.05)

        // A recorded track exports as before (`Track.gpx`) and reads back as a track.
        var recorded = Track(name: "Morning", startedAt: Date(timeIntervalSince1970: 1_780_000_000))
        recorded.points = (0..<5).map { i in
            TrackPoint(lat: -25.84 + Double(i) * 1e-4, lon: 27.39, altitude: 1500, timestamp: Date(timeIntervalSince1970: 1_780_000_000 + Double(i) * 10),
                       horizontalAccuracy: 5)
        }
        let fromTrack = try parse(recorded.gpx(), file: "Morning.gpx")
        XCTAssertEqual(fromTrack.routes.first?.kind, .gpxTrack)
        XCTAssertEqual(fromTrack.routes.first?.points.count, 5)
        XCTAssertEqual(recorded.asRoute().points.count, 5)
        XCTAssertEqual(recorded.asRoute().anchors.count, 2)
    }

    func testTheExportFileIsWrittenForSharing() throws {
        let file = GPXExportFile(name: "Bannerman/Pass: day 1", waypoints: [Waypoint(name: "Hut", position: LatLon(lat: -29.1, lon: 29.45))])
        let url = try file.write()
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertEqual(url.pathExtension, "gpx")
        XCTAssertFalse(url.lastPathComponent.contains("/"))
        let doc = try GPXService.parse(Data(contentsOf: url), fileName: url.lastPathComponent)
        XCTAssertEqual(doc.waypoints.map(\.name), ["Hut"])
    }

    func testFilesThatCannotBeRead() {
        XCTAssertThrowsError(try parse("<gpx version=\"1.1\"><wpt lat=")) { error in
            guard case GPXError.unreadable(let why) = error else { return XCTFail("\(error)") }
            XCTAssertTrue(why.hasPrefix("line"), why)
            XCTAssertTrue((error as? GPXError)?.errorDescription?.hasPrefix("This GPX file couldn't be read") ?? false)
        }
        XCTAssertThrowsError(try parse("<gpx version=\"1.1\" xmlns=\"http://www.topografix.com/GPX/1/1\"></gpx>")) { error in
            XCTAssertEqual(error as? GPXError, .empty)
        }
        XCTAssertThrowsError(try parse("<kml><Placemark/></kml>")) { error in
            XCTAssertEqual(error as? GPXError, .unreadable("it isn't a GPX file"))
        }
        XCTAssertThrowsError(try parse("not xml at all"))
        // Points without usable coordinates are left out, the rest is read.
        let doc = try? parse("""
        <gpx version="1.1"><trk><trkseg><trkpt lat="abc" lon="1"/><trkpt lat="-25.8" lon="27.4"/><trkpt lat="-25.81" lon="27.41"/></trkseg></trk></gpx>
        """, file: "Loose ends.gpx")
        XCTAssertEqual(doc?.skippedPoints, 1)
        XCTAssertEqual(doc?.routes.first?.points.count, 2)
        XCTAssertEqual(doc?.routes.first?.name, "Loose ends", "no names anywhere: the file's")
    }

    func testUnnamedItemsGetNames() throws {
        let doc = try parse("""
        <gpx version="1.1"><metadata><name>Weekend</name></metadata>
        <wpt lat="-25.8" lon="27.4"/><wpt lat="-25.81" lon="27.4"/>
        <trk><trkseg><trkpt lat="-25.8" lon="27.4"/><trkpt lat="-25.81" lon="27.41"/></trkseg></trk>
        <trk><trkseg><trkpt lat="-25.8" lon="27.4"/><trkpt lat="-25.82" lon="27.42"/></trkseg></trk>
        </gpx>
        """)
        XCTAssertEqual(doc.waypoints.map(\.name), ["Waypoint 1", "Waypoint 2"])
        XCTAssertEqual(doc.routes.map(\.name), ["Weekend", "Weekend 2"])
    }

    func testSymbols() {
        for symbol in WaypointSymbol.allCases {
            XCTAssertEqual(GPXSymbols.symbol(sym: GPXSymbols.gpxName(for: symbol)), symbol, "\(symbol)")
            XCTAssertEqual(GPXSymbols.symbol(sym: symbol.rawValue), symbol)
        }
        XCTAssertEqual(GPXSymbols.symbol(sym: "campsite"), .camp)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Spring"), .water)
        XCTAssertEqual(GPXSymbols.symbol(sym: "summit-24"), .peak)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Shelter"), .hut)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Parking Area"), .parking)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Scenic Area"), .viewpoint)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Danger Area"), .danger)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Trailhead"), .start)
        XCTAssertEqual(GPXSymbols.symbol(sym: nil, type: "Water source"), .water)
        XCTAssertEqual(GPXSymbols.symbol(sym: "Unknown thing"), .flag)
        XCTAssertEqual(GPXSymbols.symbol(sym: nil), .flag)
    }

    func testSummaryWording() {
        var s = GPXImportSummary(fileName: "Bannerman Pass.gpx")
        s.routeCount = 2
        s.waypointCount = 12
        XCTAssertEqual(s.headline, "2 routes and 12 waypoints from Bannerman Pass.gpx")
        s.trackCount = 1
        XCTAssertEqual(s.headline, "2 routes, 1 track and 12 waypoints from Bannerman Pass.gpx")
        s = GPXImportSummary(fileName: "x.gpx")
        s.waypointCount = 1
        XCTAssertEqual(s.headline, "1 waypoint from x.gpx")
        s.error = GPXError.empty.errorDescription
        XCTAssertEqual(s.headline, "This GPX file has no waypoints, routes or tracks.")
    }

    func testContentHashTellsFilesApart() {
        let a = GPXService.contentHash(Data(Self.gaia.utf8))
        XCTAssertEqual(a, GPXService.contentHash(Data(Self.gaia.utf8)))
        XCTAssertNotEqual(a, GPXService.contentHash(Data(Self.allTrails.utf8)))
        XCTAssertEqual(GPXService.contentHash(Data()), "cbf29ce484222325")
    }

    func testInboxCopiesAreRemovedOthersAreNot() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("gpx-\(UUID().uuidString)", isDirectory: true)
        let inbox = root.appendingPathComponent("Documents/Inbox", isDirectory: true)
        try FileManager.default.createDirectory(at: inbox, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let copy = inbox.appendingPathComponent("a.gpx")
        let kept = root.appendingPathComponent("b.gpx")
        try Data(Self.gaia.utf8).write(to: copy)
        try Data(Self.gaia.utf8).write(to: kept)
        GPXService.removeInboxCopy(copy)
        GPXService.removeInboxCopy(kept)
        XCTAssertFalse(FileManager.default.fileExists(atPath: copy.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: kept.path))
    }
}
