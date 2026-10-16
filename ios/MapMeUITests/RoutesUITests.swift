import XCTest

/// Routes, the elevation profile and daylight on the sample map (docs/design/explorer.md, Routes
/// and Profile; Tests and CI screenshots 30–35). Each test attaches small screenshots for the
/// lead to check on CI.
final class RoutesUITests: XCTestCase {
    /// Late afternoon on midwinter's day at Grootkloof (sunset 17:28): the daylight banner shows.
    static let afternoon = ["-mockNow", "2026-06-21T16:45:00+02:00"]

    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    /// The route menu's item (menu items are found by their title).
    private func menuItem(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.buttons[title].firstMatch
    }

    @MainActor
    func testPlanARouteOnTheSampleMap() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample"] + Self.afternoon + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }

        let routes = app.buttons["routeToolsButton"]
        XCTAssertTrue(routes.waitForExistence(timeout: 10), "No route button in the side column")
        routes.tap()
        let newRoute = menuItem(app, "New Route")
        XCTAssertTrue(newRoute.waitForExistence(timeout: 5), "No New Route in the route menu")
        newRoute.tap()
        let bar = element(app, "routeEditBar")
        XCTAssertTrue(bar.waitForExistence(timeout: 5), "The planning bar did not appear")

        // Four points across the middle of the map.
        for (dx, dy) in [(0.25, 0.38), (0.45, 0.46), (0.6, 0.42), (0.72, 0.56)] {
            app.coordinate(withNormalizedOffset: CGVector(dx: dx, dy: dy)).tap()
            Thread.sleep(forTimeInterval: 0.7)
        }
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "30-route-planning")
        XCTAssertTrue(element(app, "routeEditDistance").exists)

        // Undo the last point, then finish.
        app.buttons["routeUndoButton"].tap()
        let done = app.buttons["routeDoneButton"]
        XCTAssertTrue(done.isEnabled)
        done.tap()
        let card = element(app, "routeCard")
        XCTAssertTrue(card.waitForExistence(timeout: 5), "The new route's card did not appear")
        XCTAssertFalse(bar.exists, "The planning bar stayed")
        Thread.sleep(forTimeInterval: 2.5)
        uiAttachScreenshot(app, "31-route-card-profile")
        let expand = app.buttons["routeProfileExpandButton"]
        if expand.waitForExistence(timeout: 8) {
            expand.tap()
            XCTAssertTrue(element(app, "profileFullScreen").waitForExistence(timeout: 5))
            Thread.sleep(forTimeInterval: 1)
            uiAttachScreenshot(app, "31-route-profile-full")
            app.buttons["profileDoneButton"].tap()
        }

        // Tidy up: the route goes again.
        let more = element(app, "routeMoreButton")
        if more.waitForExistence(timeout: 3) {
            more.tap()
            let delete = app.buttons["Delete"].firstMatch
            if delete.waitForExistence(timeout: 3) {
                delete.tap()
                let confirm = app.buttons["Delete Route"].firstMatch
                if confirm.waitForExistence(timeout: 3) { confirm.tap() }
            }
        }
    }

    @MainActor
    func testFollowWithTheDaylightBanner() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-seedExplorerItems", "-followSampleRoute"] + Self.afternoon
            + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }

        let banner = element(app, "routeGuidanceBanner")
        XCTAssertTrue(banner.waitForExistence(timeout: 15), "No guidance banner while following")
        let chip = app.buttons["daylightChip"]
        XCTAssertTrue(chip.waitForExistence(timeout: 10), "No daylight line while following")
        XCTAssertTrue(chip.label.contains("daylight left"), chip.label)
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "32-following-daylight")

        chip.tap()
        XCTAssertTrue(element(app, "daylightSheet").waitForExistence(timeout: 5), "The daylight sheet did not open")
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "32-daylight-sheet")
        app.buttons["Done"].firstMatch.tap()

        // The followed route's card: the profile with you on it.
        app.buttons["routeToolsButton"].tap()
        let list = menuItem(app, "Routes…")
        if list.waitForExistence(timeout: 5) {
            list.tap()
            let row = app.buttons.matching(NSPredicate(format: "identifier == 'routesListRoute' AND label BEGINSWITH 'Hamerkop Ridge'"))
                .firstMatch
            if row.waitForExistence(timeout: 5) {
                row.tap()
                XCTAssertTrue(element(app, "routeCard").waitForExistence(timeout: 5))
                XCTAssertTrue(app.buttons["routeStopButton"].waitForExistence(timeout: 3), "Following shows Stop")
                Thread.sleep(forTimeInterval: 2)
                uiAttachScreenshot(app, "32-followed-route-card")
            }
        }
    }

    @MainActor
    func testOffRouteWarning() throws {
        let app = XCUIApplication()
        // About 1 km south-east of the seeded route, on the sheet.
        app.launchArguments = ["-importSample", "-seedExplorerItems", "-followSampleRoute", "-mockLocation",
                               "-25.8480,27.4000"] + Self.afternoon
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        // Off by more than 40 m for 20 s and two fixes: the red banner.
        let banner = element(app, "offRouteBanner")
        XCTAssertTrue(banner.waitForExistence(timeout: 40), "No off-route warning")
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "33-off-route")
    }

    @MainActor
    func testImportTheSampleGPX() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-importSampleGPX"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }

        let summary = element(app, "gpxImportSummary")
        XCTAssertTrue(summary.waitForExistence(timeout: 20), "The GPX summary did not appear")
        // Imported in an earlier run: replace what it added then.
        let replace = app.buttons["gpxReplaceButton"]
        if replace.waitForExistence(timeout: 3) {
            uiAttachScreenshot(app, "34-gpx-import-duplicate")
            replace.tap()
        }
        let show = app.buttons["gpxShowOnMapButton"]
        XCTAssertTrue(show.waitForExistence(timeout: 10), "Nothing imported")
        let headline = element(app, "gpxSummaryHeadline")
        XCTAssertTrue(headline.label.contains("1 route, 1 track and 3 waypoints"), headline.label)
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "34-gpx-import-summary")

        show.tap()
        XCTAssertTrue(element(app, "routeCard").waitForExistence(timeout: 10), "Show on Map selects the route")
        Thread.sleep(forTimeInterval: 2)
        uiAttachScreenshot(app, "34-gpx-on-map")
    }

    @MainActor
    func testTrackProfileInTheStatsPanel() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-mockWalk", "-mockWalkSpeed", "7", "-mockHeading", "35"]
            + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        // A trail to profile.
        Thread.sleep(forTimeInterval: 20)
        let panel = element(app, "statsPanel")
        XCTAssertTrue(panel.waitForExistence(timeout: 10))
        panel.swipeUp()
        Thread.sleep(forTimeInterval: 1.5)
        let profile = element(app, "trailProfile")
        XCTAssertTrue(profile.waitForExistence(timeout: 10), "No trail profile in the expanded panel")
        Thread.sleep(forTimeInterval: 3)
        uiAttachScreenshot(app, "35-track-profile")
    }
}
