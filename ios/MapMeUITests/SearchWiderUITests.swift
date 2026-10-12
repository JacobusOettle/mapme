import XCTest

/// "Search wider" through the UI, without running a wide search: the sample map is imported with a
/// simulated contour-search miss (`-simulateTerrainMiss`, Debug builds: no OCR, no core run), so
/// the result screen leads with "Search wider…"; its sheet offers 100, 250, 500, 1 000 km or a
/// custom distance and shows the core's estimate for the mocked position. Closed with Cancel.
final class SearchWiderUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
        executionTimeAllowance = 400
    }

    @MainActor
    func testSearchWiderSheet() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-simulateTerrainMiss", "-mockLocation", "-25.8405,27.3920"]
        allowLocationAlerts()
        app.launch()

        let searchWider = app.buttons["searchWiderButton"]
        guard searchWider.waitForExistence(timeout: 180) else {
            attachScreenshot(app, "07-search-wider-timeout")
            XCTFail("The import did not end on the not-found screen with Search wider")
            return
        }
        // Let the result animation settle.
        Thread.sleep(forTimeInterval: 1.5)
        attachScreenshot(app, "07-not-located")
        XCTAssertTrue(app.buttons["calibrateButton"].exists, "Calibrate manually stays on offer")
        searchWider.tap()

        let start = app.buttons["startWideSearchButton"]
        guard start.waitForExistence(timeout: 15) else {
            attachScreenshot(app, "07-search-wider-missing")
            XCTFail("The Search wider sheet did not appear")
            return
        }
        for value in [100, 250, 500, 1_000] {
            XCTAssertTrue(app.buttons["radiusOption-\(value)"].exists, "no \(value) km option")
        }
        let thousand = app.buttons["radiusOption-1000"]
        if thousand.exists { thousand.tap() }
        XCTAssertTrue(app.switches["useMapNamesToggle"].exists)

        // The estimate comes from the core (mm_search_estimate) for the mocked position.
        let estimate = app.staticTexts["searchEstimateText"]
        XCTAssertTrue(estimate.waitForExistence(timeout: 5), "no estimate line")
        let deadline = Date().addingTimeInterval(15)
        while Date() < deadline, estimate.label.hasPrefix("Working out") {
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertTrue(estimate.label.contains("MB") || estimate.label.contains("GB"), "estimate: \(estimate.label)")
        Thread.sleep(forTimeInterval: 1)
        attachScreenshot(app, "07-search-wider")

        // A custom distance brings up the number field.
        let custom = app.buttons["radiusOptionCustom"]
        if custom.exists {
            custom.tap()
            XCTAssertTrue(app.textFields["customRadiusField"].waitForExistence(timeout: 5), "no custom distance field")
        } else {
            XCTFail("no Custom option")
        }

        // Close without searching: back on the result screen.
        app.buttons["cancelSearchWiderButton"].tap()
        XCTAssertTrue(app.buttons["calibrateButton"].waitForExistence(timeout: 10))
        XCTAssertTrue(searchWider.exists)
    }

    // MARK: - Helpers

    @MainActor
    private func allowLocationAlerts() {
        addUIInterruptionMonitor(withDescription: "Location permission") { alert in
            for title in ["Allow While Using App", "Allow Once", "Allow", "OK"] {
                let button = alert.buttons[title]
                if button.exists {
                    button.tap()
                    return true
                }
            }
            return false
        }
    }

    @MainActor
    private func attachScreenshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
