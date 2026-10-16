import XCTest

/// The 3D view of the sample map (docs/design/explorer.md, 3D view; screenshots 60 and 61): the
/// side column's 3D button opens it, the terrain loads (from the network on CI, or the flat map
/// with its notice when terrain cannot be had), the camera turns and tilts, the compass faces it
/// north, the height menu changes the exaggeration, Close returns to the map.
final class Terrain3DUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func test3DViewOfTheSampleMap() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-mockHeading", "35"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }

        let button = app.buttons["terrain3DButton"]
        guard button.waitForExistence(timeout: 15) else {
            uiAttachScreenshot(app, "60-no-3d-button")
            XCTFail("The map has no 3D button")
            return
        }
        button.tap()
        let screen = app.descendants(matching: .any).matching(identifier: "terrain3DView").firstMatch
        XCTAssertTrue(screen.waitForExistence(timeout: 15), "The 3D view did not open")
        guard waitUntilBuilt(app) else { return }
        Thread.sleep(forTimeInterval: 2)
        uiAttachScreenshot(app, "60-3d-view")

        // One finger turns the view (the compass follows) and tilts it.
        let scene = app.descendants(matching: .any).matching(identifier: "terrain3DScene").firstMatch
        XCTAssertTrue(scene.exists, "The 3D scene is not on screen")
        let compass = app.buttons["terrain3DCompass"]
        XCTAssertTrue(compass.exists, "The compass is missing")
        let before = compass.label
        let start = scene.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.55))
        start.press(forDuration: 0.05, thenDragTo: scene.coordinate(withNormalizedOffset: CGVector(dx: 0.75, dy: 0.45)))
        Thread.sleep(forTimeInterval: 1.5)
        XCTAssertNotEqual(compass.label, before, "Turning the view should turn the compass")
        uiAttachScreenshot(app, "60b-3d-turned")

        // The height menu: 3× exaggeration.
        let height = app.buttons["terrain3DExaggeration"]
        if height.waitForExistence(timeout: 5) {
            height.tap()
            let three = app.buttons["3× Height"]
            if three.waitForExistence(timeout: 5) {
                three.tap()
                Thread.sleep(forTimeInterval: 1.5)
                uiAttachScreenshot(app, "60c-3d-height-3x")
            } else {
                XCTFail("The height menu has no 3× choice")
            }
        } else {
            XCTFail("The height menu is missing")
        }

        // The compass faces north again; Reset and Close.
        compass.tap()
        app.buttons["terrain3DReset"].tap()
        Thread.sleep(forTimeInterval: 1)
        app.buttons["terrain3DClose"].tap()
        XCTAssertTrue(app.buttons["trackingButton"].waitForExistence(timeout: 10), "Close did not return to the map")
    }

    /// `-open3D` opens the view by itself; `-seedExplorerItems` puts a route, waypoints and a
    /// photo note on the sheet (routes and notes features), drawn on the terrain.
    @MainActor
    func test3DViewWithARouteOpensAtLaunch() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-open3D", "-seedExplorerItems", "-mockHeading", "35"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        // The sample map shows (imported or opened), then the 3D view opens over it.
        let screen = app.descendants(matching: .any).matching(identifier: "terrain3DView").firstMatch
        let openMap = app.buttons["openMapButton"]
        let deadline = Date().addingTimeInterval(200)
        while Date() < deadline, !screen.exists {
            if openMap.exists, openMap.isHittable { openMap.tap() }
            Thread.sleep(forTimeInterval: 1)
        }
        guard screen.exists else {
            uiAttachScreenshot(app, "61-3d-not-opened")
            XCTFail("-open3D did not open the 3D view")
            return
        }
        guard waitUntilBuilt(app) else { return }
        Thread.sleep(forTimeInterval: 2)
        uiAttachScreenshot(app, "61-3d-route")
    }

    /// Waits for the build: the info capsule (terrain range and height) shows when it is done.
    @MainActor
    private func waitUntilBuilt(_ app: XCUIApplication) -> Bool {
        let info = app.descendants(matching: .any).matching(identifier: "terrain3DInfo").firstMatch
        if info.waitForExistence(timeout: 150) { return true }
        uiAttachScreenshot(app, "60-3d-not-built")
        XCTFail("The 3D view was not built within 150 s")
        return false
    }
}
