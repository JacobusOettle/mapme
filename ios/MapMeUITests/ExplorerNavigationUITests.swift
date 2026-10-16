import XCTest

/// The explorer features' entry points that exist from the start (docs/design/explorer.md): the
/// library's world view button. Each feature adds its own UI tests with screenshots.
final class ExplorerNavigationUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testWorldViewOpensFromTheLibrary() throws {
        let app = XCUIApplication()
        app.launchArguments = Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        let world = app.buttons["worldViewButton"]
        guard world.waitForExistence(timeout: 30) else {
            uiAttachScreenshot(app, "20-library-no-world-button")
            XCTFail("The library has no world view button")
            return
        }
        world.tap()
        let screen = app.descendants(matching: .any).matching(identifier: "worldView").firstMatch
        XCTAssertTrue(screen.waitForExistence(timeout: 15), "The world view did not open")
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "20-world-view")
    }
}
