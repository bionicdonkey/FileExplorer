import XCTest

final class LaunchUITests: XCTestCase {
    func testAppLaunchesWithAWindow() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10))
    }
}
