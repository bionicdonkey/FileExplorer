import XCTest

/// Drives the real app through the accessibility API, the way a person uses it: keyboard, menus and
/// the mouse. These need Developer mode and UI automation enabled once on the machine
/// (`sudo DevToolsSecurity -enable` and `sudo automationmodetool enable-automationmode-without-authentication`),
/// so they are opt-in (`UI=1 sh scripts/xcode-test.sh`) and are not part of the default run.
final class ExplorerUITests: XCTestCase {
    private var root: URL!
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        root = FileManager.default.temporaryDirectory.appendingPathComponent("ui-\(UUID().uuidString)")
        let folder = root.appendingPathComponent("folder")
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("Sub"), withIntermediateDirectories: true)
        try "hello".write(to: folder.appendingPathComponent("readme.txt"), atomically: true, encoding: .utf8)
        try "hello".write(to: folder.appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
        app = XCUIApplication()
        app.launchArguments = ["-MorinStateDirectory", root.appendingPathComponent("state").path, "-MorinOpen", folder.path]
        app.launch()
    }

    override func tearDownWithError() throws {
        app.terminate()
        try? FileManager.default.removeItem(at: root)
    }

    func testTheWindowShowsTheFolderAndItsItems() {
        let window = app.windows["explorer-window"].firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["readme.txt"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["status-text"].exists || app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'items'")).firstMatch.exists)
    }

    func testCommandTOpensANewTab() {
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10))
        app.typeKey("t", modifierFlags: .command)
        XCTAssertTrue(app.tabGroups.firstMatch.waitForExistence(timeout: 5) || app.windows.count >= 2)
    }

    func testCommandLFocusesTheLocationFieldAndATypedPathNavigates() throws {
        XCTAssertTrue(app.staticTexts["readme.txt"].waitForExistence(timeout: 10))
        app.typeKey("l", modifierFlags: .command)
        let field = app.textFields["location-field"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeKey("a", modifierFlags: .command)
        field.typeText(root.path + "\r")
        XCTAssertTrue(app.staticTexts["folder"].waitForExistence(timeout: 5))
    }

    func testSearchFiltersTheListAndEscapeRestoresIt() {
        XCTAssertTrue(app.staticTexts["readme.txt"].waitForExistence(timeout: 10))
        app.typeKey("f", modifierFlags: .command)
        let search = app.searchFields["search-field"]
        XCTAssertTrue(search.waitForExistence(timeout: 5))
        search.typeText("notes")
        XCTAssertTrue(app.staticTexts["notes.txt"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["readme.txt"].exists)
        search.typeKey(.escape, modifierFlags: [])
        XCTAssertTrue(app.staticTexts["readme.txt"].waitForExistence(timeout: 5))
    }

    func testViewMenuSwitchesTheViewMode() {
        XCTAssertTrue(app.staticTexts["readme.txt"].waitForExistence(timeout: 10))
        app.typeKey("1", modifierFlags: .command)
        XCTAssertTrue(app.tables.firstMatch.waitForExistence(timeout: 5))
        app.typeKey("3", modifierFlags: .command)
        XCTAssertTrue(app.collectionViews.firstMatch.waitForExistence(timeout: 5))
    }

    func testEveryToolbarControlHasAnAccessibilityLabel() {
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10))
        for button in app.toolbars.firstMatch.buttons.allElementsBoundByIndex where button.exists {
            XCTAssertFalse(button.label.isEmpty && button.title.isEmpty, "an unlabeled toolbar button: \(button.debugDescription)")
        }
    }
}
