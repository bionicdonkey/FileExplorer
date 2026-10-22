import XCTest
@testable import ExplorerAdapters

final class ArchiveLibraryTests: XCTestCase {
    func testLibraryIsLinked() { XCTAssertTrue(ArchiveLibrary.version.hasPrefix("libarchive")) }
}
