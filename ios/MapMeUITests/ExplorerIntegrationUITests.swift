import XCTest

/// The map screen with every explorer feature on at once (docs/design/explorer.md, Map screen):
/// the side column's five buttons (the battery saver on through `-mockLowPowerMode`), the seeded
/// route, waypoints and photo note, then a dropped pin's card over the stats panel. No control
/// may overlap another, leave the screen or run under the bottom panel, on whichever iPhone CI
/// runs (the layout's numbers for iPhone SE and Pro Max are unit-tested in
/// `ExplorerIntegrationTests`). Screenshots 70 and 71.
final class ExplorerIntegrationUITests: XCTestCase {
    private static let sideColumn = ["mapsHereButton", "terrain3DButton", "routeToolsButton", "addNoteButton",
                                     "batterySaverBadge"]

    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testEveryFeatureSharesTheMapScreen() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-mockLowPowerMode", "-seedExplorerItems"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        XCTAssertTrue(app.buttons["batterySaverBadge"].waitForExistence(timeout: 10),
                      "The battery saver's badge did not show with Low Power Mode on")
        Thread.sleep(forTimeInterval: 1.5)
        checkControlsClear(app, of: [element(app, "statsPanel")], screenshot: "70-map-screen-every-feature")

        // A dropped pin's card raises the bottom panel: the side column makes room.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.35, dy: 0.42)).press(forDuration: 1.0)
        let card = element(app, "droppedPinCard")
        guard card.waitForExistence(timeout: 5) else {
            uiAttachScreenshot(app, "71-no-dropped-pin")
            XCTFail("The long press did not drop a pin")
            return
        }
        Thread.sleep(forTimeInterval: 1)
        checkControlsClear(app, of: [card, element(app, "statsPanel")], screenshot: "71-map-screen-pin-card")
    }

    /// The side column's buttons are on screen, apart from each other and clear of `panels`.
    @MainActor
    private func checkControlsClear(_ app: XCUIApplication, of panels: [XCUIElement], screenshot: String) {
        let screen = app.windows.firstMatch.frame
        var frames: [(id: String, frame: CGRect)] = []
        for id in Self.sideColumn {
            let control = element(app, id)
            guard control.exists else {
                XCTFail("\(id) is missing from the map screen")
                continue
            }
            let frame = control.frame.insetBy(dx: 1, dy: 1)
            frames.append((id, frame))
            XCTAssertTrue(screen.contains(frame), "\(id) is off the screen: \(control.frame) in \(screen)")
        }
        for i in frames.indices {
            for j in frames.indices where j > i {
                XCTAssertFalse(frames[i].frame.intersects(frames[j].frame), "\(frames[i].id) overlaps \(frames[j].id)")
            }
        }
        for panel in panels where panel.exists {
            for control in frames {
                XCTAssertFalse(control.frame.intersects(panel.frame),
                               "\(control.id) \(control.frame) runs under the bottom panel \(panel.frame)")
            }
        }
        uiAttachScreenshot(app, screenshot)
    }

    @MainActor
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }
}
