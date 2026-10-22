import XCTest

final class LaunchUITests: XCTestCase {
    func testAppLaunchesWithAWindow() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.windows["Morin Explorer"].waitForExistence(timeout: 10))
    }
}
