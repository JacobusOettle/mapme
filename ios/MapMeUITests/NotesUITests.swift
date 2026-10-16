import XCTest

/// Notes and photos on the map (docs/design/explorer.md, Notes and photos): the seeded photo and
/// text notes on the Grootkloof sample (`-seedExplorerItems`), a note written at your position
/// through the side column, the list of notes, a photo note's card and its photo full screen.
/// Screenshots 40–43 (the camera and the photo library cannot be driven in the Simulator; the
/// seeded photo note goes through the same re-encoding and EXIF placement as a picked photo).
final class NotesUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    func testNotesAndPhotosOnTheSampleMap() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample", "-seedExplorerItems"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        // The thumbnails load and the pins settle.
        Thread.sleep(forTimeInterval: 4)
        uiAttachScreenshot(app, "41-notes-on-map")

        // A note at your position, from the side column's note button.
        let noteButton = app.buttons["addNoteButton"]
        guard noteButton.waitForExistence(timeout: 10) else {
            uiAttachScreenshot(app, "41-no-note-button")
            XCTFail("The side column has no note button")
            return
        }
        noteButton.tap()
        tapMenuItem(app, identifier: "addNoteHereButton", label: "Note Here")
        let field = element(app, "noteTextField")
        guard field.waitForExistence(timeout: 10) else {
            uiAttachScreenshot(app, "40-note-editor-missing")
            XCTFail("The note editor did not open")
            return
        }
        field.tap()
        field.typeText("Spring beside the path")
        uiAttachScreenshot(app, "40-note-editor")
        app.buttons["noteSaveButton"].tap()
        XCTAssertTrue(element(app, "noteCard").waitForExistence(timeout: 10), "The new note's card did not show")
        let title = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Spring beside the path")).firstMatch
        XCTAssertTrue(title.waitForExistence(timeout: 5), "The card does not show the note's title")
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "41b-new-note-card")

        // The seeded photo note, from the list of notes.
        noteButton.tap()
        tapMenuItem(app, identifier: "notesListButton", label: "All Notes")
        let photoRow = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Waterfall below the cliffs")).firstMatch
        guard photoRow.waitForExistence(timeout: 10) else {
            uiAttachScreenshot(app, "43-notes-list-missing")
            XCTFail("The seeded photo note is not in the list")
            return
        }
        Thread.sleep(forTimeInterval: 1)
        uiAttachScreenshot(app, "43-notes-list")
        photoRow.tap()
        XCTAssertTrue(app.staticTexts["Waterfall below the cliffs"].waitForExistence(timeout: 10),
                      "The photo note's card did not show")
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "42-photo-note-card")

        // Its photo, full screen.
        let photo = app.buttons["notePhotoButton"]
        guard photo.waitForExistence(timeout: 5) else {
            XCTFail("The photo note's card has no photo")
            return
        }
        photo.tap()
        let close = app.buttons["notePhotoClose"]
        XCTAssertTrue(close.waitForExistence(timeout: 10), "The photo viewer did not open")
        Thread.sleep(forTimeInterval: 1.5)
        uiAttachScreenshot(app, "42b-photo-viewer")
        close.tap()
    }

    /// The dropped pin offers a note there: long-press, Note, write, save.
    @MainActor
    func testANoteAtTheDroppedPin() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-importSample"] + Self.uiSampleLocation
        uiAllowSystemAlerts()
        app.launch()
        guard uiOpenSampleMap(app) != nil else { return }
        Thread.sleep(forTimeInterval: 2)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.35, dy: 0.42)).press(forDuration: 1.0)
        let note = app.buttons["dropPinNoteButton"]
        guard note.waitForExistence(timeout: 5) else {
            uiAttachScreenshot(app, "40-no-dropped-pin")
            XCTFail("The long press did not drop a pin with a Note action")
            return
        }
        note.tap()
        let field = element(app, "noteTextField")
        XCTAssertTrue(field.waitForExistence(timeout: 10), "The note editor did not open at the pin")
        field.tap()
        field.typeText("Kloof crossing")
        uiAttachScreenshot(app, "40b-note-at-pin")
        app.buttons["noteSaveButton"].tap()
        let title = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Kloof crossing")).firstMatch
        XCTAssertTrue(title.waitForExistence(timeout: 10), "The note at the pin was not saved")
    }

    // MARK: - Helpers

    @MainActor
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Taps an item of the menu that is open (by identifier, else by its title).
    @MainActor
    private func tapMenuItem(_ app: XCUIApplication, identifier: String, label: String) {
        let byID = app.buttons[identifier]
        if byID.waitForExistence(timeout: 4) {
            byID.tap()
            return
        }
        let byLabel = app.buttons[label]
        if byLabel.waitForExistence(timeout: 3) {
            byLabel.tap()
            return
        }
        uiAttachScreenshot(app, "menu-item-missing-\(identifier)")
        XCTFail("The menu has no \(label)")
    }
}
