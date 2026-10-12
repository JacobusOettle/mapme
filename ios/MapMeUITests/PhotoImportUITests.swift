import XCTest

/// Photo of a paper map, through the UI: the app imports its bundled sample photo
/// (`-importSamplePhoto`: the real phone photo of the printed Grootkloof sheet on a wooden table,
/// folded in four, which Debug builds bundle as SamplePhoto.jpg), shows "Adjust Corners" with the
/// detected sheet (in the simulator Vision often sees only the whole frame; the core's page finder
/// supplies the outline), flattens and locates it (OCR, then contours against terrain downloaded
/// over the network near the mocked GPS fix, which is inside the sheet) and opens it, drawn dark.
final class PhotoImportUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
        executionTimeAllowance = 600
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
        // The handles sit on the sheet's corners, well inside the photo (not on its corners, as
        // they would for "the whole image"): in the photo the sheet's top-left is ~12 % in from the
        // left edge and its bottom-right ~10 % in from the right; the editor fits the photo to the
        // screen width with a margin, so on screen they are ~15 % and ~13 % in.
        let width = app.frame.width
        let topLeft = app.descendants(matching: .any).matching(identifier: "cornerHandle0").firstMatch
        let bottomRight = app.descendants(matching: .any).matching(identifier: "cornerHandle2").firstMatch
        if topLeft.exists, bottomRight.exists {
            XCTAssertGreaterThan(topLeft.frame.midX, 0.1 * width, "top-left handle at \(topLeft.frame)")
            XCTAssertLessThan(bottomRight.frame.midX, 0.95 * width, "bottom-right handle at \(bottomRight.frame)")
            XCTAssertLessThan(topLeft.frame.midY, bottomRight.frame.midY)
        } else {
            XCTFail("The corner handles are not in the accessibility tree")
        }
        done.tap()

        // Flattening, OCR, the contour search and the terrain download take a while in CI.
        let openMap = app.buttons["openMapButton"]
        guard openMap.waitForExistence(timeout: 300) else {
            attachScreenshot(app, "06-photo-import-timeout")
            XCTFail("The photo was not flattened and located within 300 s")
            return
        }
        attachScreenshot(app, "05b-photo-located")
        // "Calibrate manually" is offered only when the map could not be located.
        XCTAssertFalse(app.buttons["calibrateButton"].exists, "The photo was imported but not located")
        openMap.tap()
        _ = app.buttons["trackingButton"].waitForExistence(timeout: 20)
        // Let the tiles load and the puck glide in.
        Thread.sleep(forTimeInterval: 4)
        let map = app.screenshot()
        let attachment = XCTAttachment(screenshot: map)
        attachment.name = "06-photo-map"
        attachment.lifetime = .keepAlways
        add(attachment)
        // The photo is shown dark too (the default): its paper, shadows and table tint flattened.
        if let brightness = mapBandBrightness(map) {
            XCTAssertLessThan(brightness, 0.35, "The photo map should be dark (brightness \(brightness))")
        }
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
