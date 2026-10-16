import XCTest

/// The world view with a located map (docs/design/explorer.md §7.3, World view): the sample's
/// footprint with its thumbnail, the maps list, the selected map's card and opening the map from
/// it. Screenshots "20-world-view-*".
final class WorldViewUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testWorldViewShowsTheSampleAndOpensIt() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }

        // Back to the library, then the globe.
        app.buttons["backButton"].tap()
        let globe = app.buttons["worldViewButton"]
        guard globe.waitForExistence(timeout: 15) else {
            uiAttachScreenshot(app, "20-world-view-no-globe")
            XCTFail("The library has no world view button")
            return
        }
        globe.tap()
        let screen = app.descendants(matching: .any).matching(identifier: "worldView").firstMatch
        XCTAssertTrue(screen.waitForExistence(timeout: 15), "The world view did not open")
        // Apple's tiles, the thumbnail and the terrain overlay load meanwhile.
        Thread.sleep(forTimeInterval: 5)
        uiAttachScreenshot(app, "20-world-view-maps")

        // The list: the sample is on the map.
        let header = app.buttons["worldMapsListHeader"]
        XCTAssertTrue(header.waitForExistence(timeout: 5), "The maps list is missing")
        header.tap()
        let row = app.buttons["worldLocatedRow"].firstMatch
        guard row.waitForExistence(timeout: 5) else {
            uiAttachScreenshot(app, "20-world-view-no-row")
            XCTFail("The located sample is not listed")
            return
        }
        row.tap()
        let card = app.descendants(matching: .any).matching(identifier: "worldMapCard").firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 5), "Selecting a map shows its card")
        Thread.sleep(forTimeInterval: 2)
        uiAttachScreenshot(app, "20-world-view-card")

        let open = app.buttons["worldOpenMapButton"]
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        open.tap()
        XCTAssertTrue(app.buttons["trackingButton"].waitForExistence(timeout: 15), "Open Map shows the map")
    }
}
