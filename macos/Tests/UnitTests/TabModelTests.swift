import XCTest
import ExplorerAdapters
import ExplorerDomain
@testable import MorinExplorer

/// A directory repository that answers from a fixed table, immediately and in two pages.
private final class StaticDirectories: DirectoryRepository {
    var tree: [String: [EntrySnapshot]] = [:]
    func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback) {
        guard let entries = tree[request.location.uri] else {
            return Main.deliver { done(RequestCompletion(id: request.id, outcome: .failed(OperationError(.notFound)))) }
        }
        let snapshot = DirectorySnapshot(location: ResolvedLocation(requested: request.location, resolved: request.location), entries: entries,
                                         capabilities: CapabilitySnapshot(supported: [.list, .create]))
        Main.deliver { done(RequestCompletion(id: request.id, outcome: .succeeded(snapshot))) }
    }
}

final class TabModelTests: XCTestCase {
    private var directory: URL!
    private var stateDirectory: URL!

    override func setUp() {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("tabmodel-\(UUID().uuidString)")
        directory = root.appendingPathComponent("folder")
        stateDirectory = root.appendingPathComponent("state")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() { try? FileManager.default.removeItem(at: directory.deletingLastPathComponent()) }

    private func wait(_ condition: @escaping () -> Bool, timeout: TimeInterval = 5, file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline { RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.01)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }

    private func services() -> AppServices { AppServices(arguments: ["x", "-MorinStateDirectory", stateDirectory.path]) }

    func testNavigatingAFolderShowsItsItemsSortedFoldersFirst() throws {
        try FileManager.default.createDirectory(at: directory.appendingPathComponent("beta"), withIntermediateDirectories: true)
        try "x".write(to: directory.appendingPathComponent("alpha.txt"), atomically: true, encoding: .utf8)
        try "x".write(to: directory.appendingPathComponent(".hidden"), atomically: true, encoding: .utf8)
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.path))
        wait { !tab.displayed.isEmpty }
        XCTAssertEqual(tab.displayed.map(\.name), ["beta", "alpha.txt"])
        XCTAssertEqual(tab.state.history.current, Location(localPath: directory.path))
    }

    func testAMissingFolderReportsAReasonAndKeepsTheTabUsable() {
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.appendingPathComponent("gone").path))
        wait { tab.failureMessage != nil }
        XCTAssertTrue(tab.failureMessage!.contains("no longer exists"))
        XCTAssertFalse(tab.canGoBack)
    }

    func testHiddenFilesAppearWhenTheOptionIsOn() throws {
        try "x".write(to: directory.appendingPathComponent(".dot"), atomically: true, encoding: .utf8)
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.path))
        wait { !tab.isLoading && tab.state.directory != nil }
        XCTAssertTrue(tab.displayed.isEmpty)
        tab.updateOptions { $0.showHidden = true }
        XCTAssertEqual(tab.displayed.map(\.name), [".dot"])
    }

    func testViewOptionsAreRememberedPerFolder() throws {
        let services = services()
        let tab = TabModel(location: Location(localPath: directory.path), services: services)
        tab.navigate(to: Location(localPath: directory.path))
        wait { tab.state.directory != nil }
        wait { !tab.isLoading }
        RunLoop.main.run(until: Date().addingTimeInterval(0.15))
        tab.updateOptions { $0.viewMode = .iconGrid; $0.sortField = .size }
        let other = TabModel(location: Location(localPath: directory.path), services: services)
        other.navigate(to: Location(localPath: directory.path))
        wait { other.options.viewMode == .iconGrid }
        XCTAssertEqual(other.options.viewMode, .iconGrid)
        XCTAssertEqual(other.options.sortField, .size)
    }

    func testBackAndUpWalkTheHistory() throws {
        let child = directory.appendingPathComponent("child")
        try FileManager.default.createDirectory(at: child, withIntermediateDirectories: true)
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.path))
        wait { tab.state.directory != nil }
        tab.navigate(to: Location(localPath: child.path))
        wait { tab.location == Location(localPath: child.path) && tab.state.directory != nil }
        XCTAssertTrue(tab.canGoBack)
        XCTAssertEqual(tab.parent, Location(localPath: directory.path))
        tab.back()
        wait { tab.location == Location(localPath: self.directory.path) }
        XCTAssertTrue(tab.canGoForward)
    }

    func testSearchFindsMatchesAndClearingRestoresTheFolder() throws {
        try "x".write(to: directory.appendingPathComponent("report-1.txt"), atomically: true, encoding: .utf8)
        try "x".write(to: directory.appendingPathComponent("other.txt"), atomically: true, encoding: .utf8)
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.path))
        wait { tab.displayed.count == 2 }
        tab.beginSearch("report", mode: .filename)
        wait { tab.displayed.map(\.name) == ["report-1.txt"] }
        XCTAssertTrue(tab.statusText.contains("1 match"))
        tab.endSearch()
        XCTAssertEqual(tab.displayed.count, 2)
    }

    func testSelectionDrivesCommandAvailability() throws {
        try "x".write(to: directory.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        let tab = TabModel(location: Location(localPath: directory.path), services: services())
        tab.navigate(to: Location(localPath: directory.path))
        wait { tab.displayed.count == 1 }
        XCTAssertFalse(tab.availability.canCopy)
        tab.setSelection(tab.displayed)
        XCTAssertTrue(tab.availability.canCopy && tab.availability.canTrash && tab.availability.canRename)
        XCTAssertTrue(tab.statusText.contains("1 selected"))
    }
}
