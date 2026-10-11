import XCTest

/// Photo of a paper map, through the UI: the app photographs its bundled sample sheet
/// (`-importSamplePhoto`, a synthetic photo in perspective on a table), shows "Adjust Corners"
/// with the detected sheet, flattens and locates it, and opens it with a mocked GPS fix.
final class PhotoImportUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
        executionTimeAllowance = 480
    }

    @MainActor
    func testPhotoOfPaperMap() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSamplePhoto", "-mockLocation", "-25.8405,27.3920", "-mockHeading", "35"]
        allowLocationAlerts()
        app.launch()

        let done = app.buttons["cornersDoneButton"]
        guard done.waitForExistence(timeout: 120) else {
            attachScreenshot(app, "05-photo-corners-timeout")
            XCTFail("The corner editor did not appear")
            return
        }
        // Let the outline and handles settle.
        Thread.sleep(forTimeInterval: 1.5)
        attachScreenshot(app, "05-photo-corners")
        XCTAssertTrue(app.buttons["cornersAutoButton"].exists)
        XCTAssertTrue(app.buttons["cornersFullImageButton"].exists)
        XCTAssertTrue(done.isEnabled)
        done.tap()

        let openMap = app.buttons["openMapButton"]
        guard openMap.waitForExistence(timeout: 240) else {
            attachScreenshot(app, "06-photo-import-timeout")
            XCTFail("The photo was not flattened and located within 240 s")
            return
        }
        attachScreenshot(app, "05b-photo-located")
        openMap.tap()
        Thread.sleep(forTimeInterval: 4)
        attachScreenshot(app, "06-photo-map")
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
