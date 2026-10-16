import UIKit
import XCTest

/// Helpers for the explorer features' UI tests (docs/design/explorer.md, Tests): open the bundled
/// sample map, attach small screenshots the lead checks on CI, let permission alerts through.
/// Names carry a `ui` prefix so they never clash with the older tests' private helpers.
extension XCTestCase {
    /// Inside the Grootkloof sheet (Magaliesberg), as `ScreenshotTests`.
    static var uiSampleLocation: [String] { ["-mockLocation", "-25.8405,27.3920"] }
    /// About 0.8 km north of the sheet's top edge: off the map, in the terrain continuing it.
    static var uiOffMapLocation: [String] { ["-mockLocation", "-25.7990,27.4000"] }

    /// A JPEG screenshot (about a tenth of a PNG's size), kept in the result bundle.
    @MainActor
    func uiAttachScreenshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(image: app.screenshot().image, quality: .medium)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Location, notifications, camera and photos alerts: allow (mock modes never ask, a real
    /// prompt must not block the run).
    @MainActor
    func uiAllowSystemAlerts() {
        addUIInterruptionMonitor(withDescription: "Permission alerts") { alert in
            for title in ["Allow While Using App", "Allow Once", "Allow", "OK", "Allow Full Access"] {
                let button = alert.buttons[title]
                if button.exists {
                    button.tap()
                    return true
                }
            }
            return false
        }
    }

    /// Imports and locates the sample (launch with `-importSample`) or opens it (imported before),
    /// then waits for the map screen. Returns its tracking button, or nil (with a failure and a
    /// screenshot).
    @MainActor
    func uiOpenSampleMap(_ app: XCUIApplication, timeout: TimeInterval = 180) -> XCUIElement? {
        let openMap = app.buttons["openMapButton"]
        let trackingButton = app.buttons["trackingButton"]
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if openMap.exists, openMap.isHittable {
                openMap.tap()
                break
            }
            if trackingButton.exists { break }
            Thread.sleep(forTimeInterval: 1)
        }
        guard trackingButton.waitForExistence(timeout: 15) else {
            uiAttachScreenshot(app, "sample-map-timeout")
            XCTFail("The sample map did not open within \(Int(timeout)) s")
            return nil
        }
        return trackingButton
    }
}
