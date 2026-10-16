import XCTest

/// Offline terrain through the UI (docs/design/explorer.md, Offline regions), without a real
/// download: `-mockOfflineDownloads` serves every tile from a fake server (a small PNG after
/// 60 ms, sizes for the estimate's HEAD requests) into a store of its own that is emptied at
/// launch, and `-mockRegionPlace` answers the place search instead of Apple Maps. Settings →
/// Offline terrain → Add Region…: search Tasmania, see the measured estimate, download, watch the
/// progress in the list, delete it with a swipe.
final class OfflineRegionsUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
        executionTimeAllowance = 300
    }

    @MainActor
    func testPickARegionDownloadAndDelete() throws {
        let app = XCUIApplication()
        app.launchArguments = Self.uiSampleLocation + ["-mockOfflineDownloads",
                                                       "-mockRegionPlace", "Tasmania=-43.6,145.5,-40.7,148.5"]
        uiAllowSystemAlerts()
        app.launch()

        // Settings → Offline terrain.
        let settings = app.buttons["settingsButton"]
        guard settings.waitForExistence(timeout: 30) else {
            uiAttachScreenshot(app, "50-offline-no-settings")
            XCTFail("The library has no Settings button")
            return
        }
        settings.tap()
        let add = app.buttons["addOfflineRegionButton"]
        guard scrollSettings(app, toReveal: add) else {
            uiAttachScreenshot(app, "50-offline-section-missing")
            XCTFail("Settings has no Offline terrain section")
            return
        }
        XCTAssertTrue(app.switches["keepTerrainAroundMapsToggle"].exists, "no switch for the terrain around maps")
        uiAttachScreenshot(app, "50-offline-terrain-settings")
        add.tap()

        // The region picker: search a place, pick it, wait for the measured estimate.
        let field = app.textFields["regionSearchField"]
        guard field.waitForExistence(timeout: 15) else {
            uiAttachScreenshot(app, "51-region-picker-missing")
            XCTFail("The region picker did not open")
            return
        }
        field.tap()
        field.typeText("Tasmania\n")
        let result = app.buttons["regionResult-0"]
        guard result.waitForExistence(timeout: 10) else {
            uiAttachScreenshot(app, "51-region-search-no-result")
            XCTFail("The place search found nothing")
            return
        }
        result.tap()
        let estimate = app.descendants(matching: .any).matching(identifier: "regionEstimateText").firstMatch
        XCTAssertTrue(estimate.waitForExistence(timeout: 10), "no estimate line")
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline, !(estimate.label.contains("tiles")), !(estimate.label.contains("Couldn")) {
            Thread.sleep(forTimeInterval: 0.5)
        }
        // "≈ 103 MB · 2 100 tiles · measured" (the fake server's sizes), Wi-Fi recommended.
        XCTAssertTrue(estimate.label.contains("measured"), "estimate: \(estimate.label)")
        XCTAssertTrue(estimate.label.contains("MB") || estimate.label.contains("GB"), "estimate: \(estimate.label)")
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "51-region-picker-estimate")
        let download = app.buttons["downloadRegionButton"]
        XCTAssertTrue(download.isEnabled, "Download is off: \(estimate.label)")
        download.tap()

        // Back in Settings: the region downloads, with progress.
        let row = app.descendants(matching: .any).matching(identifier: "offlineRegionRow-0").firstMatch
        guard row.waitForExistence(timeout: 15) else {
            uiAttachScreenshot(app, "52-offline-row-missing")
            XCTFail("The new region is not listed")
            return
        }
        let status = row.staticTexts["offlineRegionStatus"]
        let started = Date().addingTimeInterval(10)
        while Date() < started, !status.label.contains("tiles") {
            Thread.sleep(forTimeInterval: 0.3)
        }
        XCTAssertTrue(status.label.contains("tiles") || status.label.contains("MB"), "status: \(status.label)")
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "52-offline-downloading")

        // Pause and resume from the row's button.
        let pause = row.buttons["pauseOfflineRegionButton"]
        if pause.exists, pause.isHittable {
            pause.tap()
            XCTAssertTrue(row.buttons["resumeOfflineRegionButton"].waitForExistence(timeout: 5), "no Resume after Pause")
            XCTAssertTrue(status.label.contains("Paused"), "status: \(status.label)")
            row.buttons["resumeOfflineRegionButton"].tap()
        }

        // Swipe to delete: the list is empty again.
        row.swipeLeft()
        let delete = app.buttons["deleteOfflineRegionButton"].exists ? app.buttons["deleteOfflineRegionButton"] : app.buttons["Delete"]
        if delete.waitForExistence(timeout: 5) {
            delete.tap()
        }
        let gone = Date().addingTimeInterval(10)
        while Date() < gone, row.exists {
            Thread.sleep(forTimeInterval: 0.3)
        }
        XCTAssertFalse(row.exists, "the region was not deleted")
    }

    /// Swipes the Settings form up until `element` can be tapped (a form only builds the rows on
    /// screen).
    @MainActor
    private func scrollSettings(_ app: XCUIApplication, toReveal element: XCUIElement) -> Bool {
        for _ in 0..<10 {
            if element.exists, element.isHittable { return true }
            let form = app.collectionViews.firstMatch.exists ? app.collectionViews.firstMatch : app.tables.firstMatch
            if form.exists {
                form.swipeUp(velocity: .slow)
            } else {
                app.swipeUp()
            }
        }
        return element.exists && element.isHittable
    }
}
