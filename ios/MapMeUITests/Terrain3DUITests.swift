import XCTest

/// The 3D view of the sample map (docs/design/explorer.md, 3D view; screenshots 60 and 61): the
/// side column's 3D button opens it, the terrain loads (from the network on CI, or the flat map
/// with its notice when terrain cannot be had), the camera turns and tilts, the compass faces it
/// north, the height menu changes the exaggeration, the appearance button switches to paper and
/// back keeping the view, Close returns to the map.
final class Terrain3DUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func test3DViewOfTheSampleMap() throws {
        let app = XCUIApplication()
        // Dark for this run only: the switch to paper below is not saved, later tests start dark.
        app.launchArguments = ["-importSample", "-mockHeading", "35", "-mapAppearance", "dark"] + Self.uiSampleLocation
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
                func heightIsThree() -> Bool {
                    let chosen = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "3× Height"),
                                                           object: height)
                    return XCTWaiter().wait(for: [chosen], timeout: 3) == .completed
                }
                three.tap()
                // A tap while the menu is still opening can miss it: tap again while it is open.
                if !heightIsThree(), three.exists { three.tap() }
                XCTAssertTrue(heightIsThree(), "Choosing 3× should set the height")
                Thread.sleep(forTimeInterval: 1.5)
                uiAttachScreenshot(app, "60c-3d-height-3x")
            } else {
                XCTFail("The height menu has no 3× choice")
            }
        } else {
            XCTFail("The height menu is missing")
        }

        // Paper (the map's Layers → Map Appearance, one setting): rebuilt with the old block on
        // screen, the camera and the height kept; then dark again.
        let appearance = app.buttons["terrain3DAppearance"]
        if appearance.waitForExistence(timeout: 5) {
            let view = compass.label
            appearance.tap()
            XCTAssertTrue(waitFor(appearance, labelContaining: "Paper"), "The appearance button should say Paper")
            XCTAssertFalse(app.descendants(matching: .any).matching(identifier: "terrain3DLoading").firstMatch.exists,
                           "Switching the appearance should keep the 3D view on screen")
            if waitUntilBuilt(app, showing: "Paper") {
                Thread.sleep(forTimeInterval: 1.5)
                uiAttachScreenshot(app, "60d-3d-paper")
                XCTAssertEqual(compass.label, view, "Switching the appearance should keep the view")
            }
            appearance.tap()
            XCTAssertTrue(waitFor(appearance, labelContaining: "Dark"), "The appearance button should say Dark")
            _ = waitUntilBuilt(app, showing: "Dark")
        } else {
            XCTFail("The appearance button is missing")
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
    /// With `appearance` ("Dark", "Paper"), first for the scene to show it: a switch rebuilds the
    /// block while the old one stays on screen.
    @MainActor
    private func waitUntilBuilt(_ app: XCUIApplication, showing appearance: String? = nil) -> Bool {
        if let appearance {
            let scene = app.descendants(matching: .any).matching(identifier: "terrain3DScene").firstMatch
            let shown = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", appearance), object: scene)
            guard XCTWaiter().wait(for: [shown], timeout: 150) == .completed else {
                uiAttachScreenshot(app, "60-3d-not-switched")
                XCTFail("The 3D view did not switch to \(appearance) within 150 s")
                return false
            }
        }
        let info = app.descendants(matching: .any).matching(identifier: "terrain3DInfo").firstMatch
        if info.waitForExistence(timeout: 150) { return true }
        uiAttachScreenshot(app, "60-3d-not-built")
        XCTFail("The 3D view was not built within 150 s")
        return false
    }

    @MainActor
    private func waitFor(_ element: XCUIElement, labelContaining text: String, timeout: TimeInterval = 5) -> Bool {
        let said = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", text), object: element)
        return XCTWaiter().wait(for: [said], timeout: timeout) == .completed
    }
}
