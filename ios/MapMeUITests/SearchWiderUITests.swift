import XCTest

/// "Search wider" through the UI, without running a wide search: the sample map is imported with a
/// simulated contour-search miss (`-simulateTerrainMiss`, Debug builds: no OCR, no core run), so
/// the result screen leads with "Search wider…"; its sheet offers 100, 250, 500, 1 000 km or a
/// custom distance and shows the core's estimate for the mocked position. Closed with Cancel.
/// The names on the map are looked up in a fixed table (`-mockPlaces`), not Apple Maps, and the
/// sheet's list is scrolled to each row it checks: a List only builds the rows on screen, and the
/// places section above the distances pushes the lower rows off it.
final class SearchWiderUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
        executionTimeAllowance = 400
    }

    @MainActor
    func testSearchWiderSheet() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-simulateTerrainMiss", "-mockLocation", "-25.8405,27.3920",
                               "-mockPlaces", "Magaliesberg=-25.80,27.45;Hartbeespoort=-25.74,27.85"]
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
        let useNames = app.switches["useMapNamesToggle"]
        scrollSheet(app, toReveal: useNames)
        XCTAssertTrue(useNames.exists, "no \u{201C}Use names on the map\u{201D} switch")

        // The estimate comes from the core (mm_search_estimate) for the mocked position.
        let estimate = app.staticTexts["searchEstimateText"]
        scrollSheet(app, toReveal: estimate)
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
        scrollSheet(app, toReveal: custom)
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

    /// A map of somewhere far away: the names on it are found ~430 km from you (`-mockPlaces`
    /// stands in for Apple Maps), so the sheet says how far away they are and picks 500 km, which
    /// reaches them, instead of the default 250 km.
    @MainActor
    func testSearchWiderSaysWhereTheNamedPlacesAreAndReachesThem() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-simulateTerrainMiss", "-mockLocation", "-25.8405,27.3920",
                               "-mockPlaces", "Grootkloof=-29.2678,29.5213;Magaliesberg=-29.2167,29.4667"]
        allowLocationAlerts()
        app.launch()

        let searchWider = app.buttons["searchWiderButton"]
        guard searchWider.waitForExistence(timeout: 180) else {
            attachScreenshot(app, "07b-search-wider-timeout")
            XCTFail("The import did not end on the not-found screen with Search wider")
            return
        }
        Thread.sleep(forTimeInterval: 1.5)
        searchWider.tap()

        let place = app.descendants(matching: .any).matching(identifier: "namedPlace-0").firstMatch
        guard place.waitForExistence(timeout: 20) else {
            attachScreenshot(app, "07b-named-places-missing")
            XCTFail("The sheet does not list the places named on the map")
            return
        }
        XCTAssertTrue(place.label.contains("km away"), "place row: \(place.label)")
        XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "namedPlace-1").firstMatch.exists)
        let fiveHundred = app.buttons["radiusOption-500"]
        let deadline = Date().addingTimeInterval(10)
        while !fiveHundred.isSelected, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.3)
        }
        XCTAssertTrue(fiveHundred.isSelected, "500 km (reaching the places ~430 km away) is not picked")
        XCTAssertFalse(app.buttons["radiusOption-250"].isSelected)
        let footer = app.staticTexts["namedPlacesFooter"]
        XCTAssertTrue(footer.exists && footer.label.contains("checks these places first"), "footer: \(footer.label)")
        Thread.sleep(forTimeInterval: 1)
        attachScreenshot(app, "07b-search-wider-far-places")

        // A shorter distance no longer reaches them, and the footer says what does.
        let shorter = app.buttons["radiusOption-250"]
        if shorter.exists { shorter.tap() }
        XCTAssertTrue(footer.waitForExistence(timeout: 5))
        XCTAssertTrue(footer.label.contains("Pick 500 km or more"), "footer: \(footer.label)")
        app.buttons["cancelSearchWiderButton"].tap()
        XCTAssertTrue(app.buttons["calibrateButton"].waitForExistence(timeout: 10))
    }

    // MARK: - Helpers

    /// Drags the sheet's list up until `element` is on screen (a List builds only the rows it
    /// shows), at most `maxDrags` times. Drags in the middle of the window: the sheet's list, clear
    /// of its Start bar and of the screen behind it.
    @MainActor
    private func scrollSheet(_ app: XCUIApplication, toReveal element: XCUIElement, maxDrags: Int = 4) {
        let window = app.windows.firstMatch
        var drags = 0
        while !element.exists, drags < maxDrags {
            let from = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.7))
            let to = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4))
            from.press(forDuration: 0.05, thenDragTo: to)
            drags += 1
        }
    }

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
