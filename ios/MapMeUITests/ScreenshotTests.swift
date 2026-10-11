import XCTest

/// Drives the app on the bundled Grootkloof sample with a mocked GPS walk and attaches screenshots
/// (kept in the .xcresult; CI exports them as artifacts).
final class ScreenshotTests: XCTestCase {
    /// Inside the Grootkloof sheet (Magaliesberg), walking a loop at 7 m/s so a long trail builds up quickly.
    static let mockArguments = ["-mockLocation", "-25.8405,27.3920", "-mockHeading", "35", "-mockWalk",
                                "-mockWalkSpeed", "7"]

    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testLibraryAppears() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-mockLocation", "-25.8405,27.3920"]
        app.launch()
        let library = app.navigationBars["MapMe"].waitForExistence(timeout: 30)
            || app.buttons["importMenu"].waitForExistence(timeout: 5)
        attachScreenshot(app, "00-library")
        XCTAssertTrue(library, "The map library did not appear")
    }

    @MainActor
    func testMapScreenshots() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample"] + Self.mockArguments
        allowLocationAlerts()
        app.launch()

        // Import + locate the sample (fresh install), or go straight to the map if it was imported before.
        let openMap = app.buttons["openMapButton"]
        let trackingButton = app.buttons["trackingButton"]
        let deadline = Date().addingTimeInterval(180)
        var reachedMap = false
        while Date() < deadline {
            if openMap.exists, openMap.isHittable {
                openMap.tap()
                reachedMap = true
                break
            }
            if trackingButton.exists {
                reachedMap = true
                break
            }
            Thread.sleep(forTimeInterval: 1)
        }
        guard reachedMap else {
            attachScreenshot(app, "00-import-timeout")
            XCTFail("The sample map was not imported within 180 s")
            return
        }
        guard trackingButton.waitForExistence(timeout: 15) else {
            attachScreenshot(app, "00-no-map")
            XCTFail("trackingButton not found")
            return
        }

        // Follow mode (north up), with a long dotted trail walked behind the puck.
        setTracking(trackingButton, to: "follow")
        Thread.sleep(forTimeInterval: 15)
        attachScreenshot(app, "01-map")

        // Follow with heading: the map turns with the walker and the compass appears.
        setTracking(trackingButton, to: "heading")
        Thread.sleep(forTimeInterval: 3)
        XCTAssertTrue(app.buttons["compassButton"].waitForExistence(timeout: 5), "The compass did not appear in heading mode")
        attachScreenshot(app, "02-heading")

        // Expand the stats panel.
        let nearby = app.buttons["nearbyButton"]
        let panel = app.descendants(matching: .any).matching(identifier: "statsPanel").firstMatch
        if panel.waitForExistence(timeout: 10) {
            panel.swipeUp()
            Thread.sleep(forTimeInterval: 1.5)
        } else {
            XCTFail("statsPanel not found")
        }
        if !nearby.waitForExistence(timeout: 3) {
            // Drag the panel's top edge up by hand.
            let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.86))
            start.press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.45)))
            Thread.sleep(forTimeInterval: 1.5)
        }
        attachScreenshot(app, "03-panel")

        if nearby.waitForExistence(timeout: 10) {
            nearby.tap()
            Thread.sleep(forTimeInterval: 2)
        } else {
            XCTFail("nearbyButton not found")
        }
        attachScreenshot(app, "04-nearby")
    }

    // MARK: - Helpers

    /// Taps the tracking button (none → follow → heading → follow) until it reports `value`.
    @MainActor
    private func setTracking(_ button: XCUIElement, to value: String) {
        for _ in 0..<4 {
            if (button.value as? String) == value { return }
            button.tap()
            Thread.sleep(forTimeInterval: 0.8)
        }
        XCTAssertEqual(button.value as? String, value, "Tracking mode did not reach \(value)")
    }

    /// Mock mode never asks, but a real permission alert must not block the run.
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
