import XCTest

/// The maps feature on the map screen (docs/design/explorer.md §7.3): "Maps here" with two maps
/// over the same ground, walking off the sample sheet into the terrain beyond it, and switching
/// to the more detailed map automatically. Screenshots 21, 22 and 23.
final class MapsUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    /// Waits until "Maps here" counts `count` maps (the overview is cloned once the sample is
    /// located).
    @MainActor
    private func waitForMapsHere(_ app: XCUIApplication, count: Int, timeout: TimeInterval) -> XCUIElement? {
        let button = app.buttons["mapsHereButton"]
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if button.exists, (button.value as? String) == "\(count)" { return button }
            Thread.sleep(forTimeInterval: 1)
        }
        return nil
    }

    @MainActor
    func testMapsHereListsBothMaps() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-seedSecondSampleMap"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        guard let button = waitForMapsHere(app, count: 2, timeout: 60) else {
            uiAttachScreenshot(app, "21-maps-here-missing")
            XCTFail("Maps here does not count the sample and its overview")
            return
        }
        button.tap()
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "21-maps-here")
        XCTAssertTrue(app.buttons["mapsHereWorldView"].waitForExistence(timeout: 5)
                          || app.buttons["World View"].exists, "The menu offers the world view")
    }

    @MainActor
    func testWalkingOffTheSampleShowsTheTerrainBeyondIt() throws {
        let app = XCUIApplication()
        // About 0.8 km north of the sheet's top edge.
        app.launchArguments = ["-importSample"] + Self.uiOffMapLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        let banner = app.descendants(matching: .any).matching(identifier: "walkedOffBanner").firstMatch
        XCTAssertTrue(banner.waitForExistence(timeout: 20), "The walked-off banner did not appear")
        // The camera glides to you; terrain tiles load.
        Thread.sleep(forTimeInterval: 5)
        uiAttachScreenshot(app, "22-off-map-terrain")
        XCTAssertTrue(app.buttons["walkedOffWorldButton"].exists, "The banner offers the world view")
    }

    @MainActor
    func testSwitchesToTheMoreDetailedMapAutomatically() throws {
        // First run: the sample is imported and located, and its overview cloned.
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-seedSecondSampleMap"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        guard waitForMapsHere(app, count: 2, timeout: 60) != nil else {
            uiAttachScreenshot(app, "23-auto-switch-no-overview")
            XCTFail("The overview map was not made")
            return
        }
        app.terminate()

        // Second run: open the overview (¼ of the detail) while following you on the sample's
        // ground. After the 30 s grace of an opened map it switches to the sample, 4× finer.
        app.launchArguments = ["-openSampleOverview"] + Self.uiSampleLocation
        app.launch()
        let chip = app.buttons["georefChip"]
        guard chip.waitForExistence(timeout: 60) else {
            uiAttachScreenshot(app, "23-auto-switch-no-map")
            XCTFail("The overview did not open")
            return
        }
        XCTAssertTrue(chip.label.contains("Grootkloof overview"), "Opened \(chip.label)")
        let toast = app.descendants(matching: .any).matching(identifier: "mapToast").firstMatch
        guard toast.waitForExistence(timeout: 90) else {
            uiAttachScreenshot(app, "23-auto-switch-none")
            XCTFail("No automatic switch within 90 s")
            return
        }
        uiAttachScreenshot(app, "23-auto-switch-toast")
        XCTAssertTrue(chip.label.contains("Grootkloof sample"), "Switched to \(chip.label)")
    }
}
