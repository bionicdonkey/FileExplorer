import XCTest
import ExplorerDomain
import ExplorerAdapters

final class LaunchTests: XCTestCase {
    func testPackagesLink() {
        XCTAssertEqual(ExplorerDomainInfo.specVersion, 1)
        XCTAssertTrue(ArchiveLibrary.version.hasPrefix("libarchive"))
    }
}
