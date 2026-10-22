import XCTest
@testable import ExplorerDomain

final class VersionTests: XCTestCase {
    func testSpecVersion() { XCTAssertEqual(ExplorerDomainInfo.specVersion, 1) }
}
