import XCTest
@testable import ExplorerDomain

final class LocationCompletionTests: XCTestCase {
    func testALocalPathSplitsIntoFolderAndPrefix() {
        let target = CompletionTarget("/Users/ada/Doc")!
        XCTAssertEqual(target.directoryText, "/Users/ada/")
        XCTAssertEqual(target.prefix, "Doc")
        XCTAssertEqual(target.directory, Location(localPath: "/Users/ada"))
        XCTAssertEqual(CompletionTarget("/")?.prefix, "")
    }

    func testAURISpellingIsKeptIntact() {
        let target = CompletionTarget("smb://host/share/Re%20p")!
        XCTAssertEqual(target.directoryText, "smb://host/share/")
        XCTAssertEqual(target.prefix, "Re p")
    }

    func testUnsupportedTextHasNoTarget() {
        for text in ["", "relative/path", "smb://host", "/tmp/a\nb", "/tmp/a?x", "/tmp/a#x"] { XCTAssertNil(CompletionTarget(text), text) }
    }

    func testMatchingIsCaseInsensitiveAndHidesDotfiles() {
        let target = CompletionTarget("/tmp/do")!
        XCTAssertTrue(target.matches("Documents"))
        XCTAssertTrue(target.matches("downloads"))
        XCTAssertFalse(target.matches("Desktop"))
        XCTAssertFalse(CompletionTarget("/tmp/")!.matches(".hidden"))
        XCTAssertTrue(CompletionTarget("/tmp/.h")!.matches(".hidden"))
    }

    func testSuggestionsAppendASeparatorAndEscapeURISegments() {
        XCTAssertEqual(CompletionTarget("/tmp/D")!.suggestion(for: "Documents").text, "/tmp/Documents/")
        XCTAssertEqual(CompletionTarget("smb://h/s/R")!.suggestion(for: "Report 1").text, "smb://h/s/Report%201/")
    }

    func testSuggestionsAreSortedAndCapped() {
        let names = (0..<80).map { "d\($0)" } + ["Alpha", "alpha2", "other"]
        let found = CompletionTarget("/tmp/")!.suggestions(from: names)
        XCTAssertEqual(found.count, CompletionTarget.maximumSuggestions)
        XCTAssertEqual(found.first?.name, "Alpha")
        XCTAssertEqual(CompletionTarget("/tmp/d1")!.suggestions(from: names).map(\.name).first, "d1")
    }
}
