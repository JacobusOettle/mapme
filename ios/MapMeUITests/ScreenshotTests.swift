import XCTest

/// Drives the app on the bundled Grootkloof sample with a mocked GPS fix and attaches screenshots
/// (kept in the .xcresult; CI exports them as artifacts).
final class ScreenshotTests: XCTestCase {
    /// Inside the Grootkloof sheet (Magaliesberg).
    static let mockArguments = ["-mockLocation", "-25.8405,27.3920", "-mockHeading", "35", "-mockWalk"]

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
        let orientation = app.buttons["orientationButton"]
        let deadline = Date().addingTimeInterval(180)
        var reachedMap = false
        while Date() < deadline {
            if openMap.exists, openMap.isHittable {
                openMap.tap()
                reachedMap = true
                break
            }
            if orientation.exists {
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

        // Let tiles load and the mock walk start.
        Thread.sleep(forTimeInterval: 4)
        attachScreenshot(app, "01-map-north-up")

        if orientation.waitForExistence(timeout: 10) {
            orientation.tap()
            Thread.sleep(forTimeInterval: 2.5)
        } else {
            XCTFail("orientationButton not found")
        }
        attachScreenshot(app, "02-heading-up")

        let nearby = app.buttons["nearbyButton"]
        if nearby.waitForExistence(timeout: 10) {
            nearby.tap()
            Thread.sleep(forTimeInterval: 2)
        } else {
            XCTFail("nearbyButton not found")
        }
        attachScreenshot(app, "03-nearby")
    }

    // MARK: - Helpers

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
