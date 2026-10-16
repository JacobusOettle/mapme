import XCTest

/// The battery saver (docs/design/explorer.md, Battery saver), with Low Power Mode mocked on
/// (`-mockLowPowerMode`, the default mode "With Low Power Mode"): the leaf badge in the map's side
/// column and its popover, Settings → Battery and what it changes. Screenshots 45.
final class BatteryUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testBatterySaverSettings() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-mockLowPowerMode"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        let settings = app.buttons["settingsButton"]
        guard settings.waitForExistence(timeout: 30) else {
            uiAttachScreenshot(app, "45-no-settings-button")
            XCTFail("The library has no Settings button")
            return
        }
        settings.tap()
        // Scroll the Battery section fully into view (it follows Display, Maps and Routes).
        let details = element(app, "batterySaverDetailsLink")
        for _ in 0..<8 {
            if details.exists, details.isHittable { break }
            app.swipeUp()
            Thread.sleep(forTimeInterval: 0.6)
        }
        XCTAssertTrue(element(app, "batterySaverPicker").exists, "Settings has no battery saver picker")
        let status = element(app, "batterySaverStatus")
        XCTAssertTrue(status.exists, "Settings does not say whether the saver is on")
        let saysOn = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Low Power Mode is on")).firstMatch
        XCTAssertTrue(saysOn.waitForExistence(timeout: 5), "With Low Power Mode on, the saver should say it is on")
        Thread.sleep(forTimeInterval: 0.5)
        uiAttachScreenshot(app, "45-battery-saver-settings")

        guard details.exists else {
            XCTFail("No 'What it changes' link")
            return
        }
        details.tap()
        XCTAssertTrue(app.navigationBars["Battery Saver"].waitForExistence(timeout: 5), "The details page did not open")
        Thread.sleep(forTimeInterval: 0.8)
        uiAttachScreenshot(app, "45b-battery-saver-details")
    }

    @MainActor
    func testBatterySaverBadgeOnTheMap() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-mockLowPowerMode"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        let badge = app.buttons["batterySaverBadge"]
        guard badge.waitForExistence(timeout: 10) else {
            uiAttachScreenshot(app, "45-no-battery-badge")
            XCTFail("The battery saver badge did not show with Low Power Mode on")
            return
        }
        badge.tap()
        XCTAssertTrue(element(app, "batterySaverPopover").waitForExistence(timeout: 5)
                      || app.staticTexts["Battery Saver"].waitForExistence(timeout: 3),
                      "The badge's popover did not open")
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "45c-battery-saver-badge")
    }

    @MainActor
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }
}
