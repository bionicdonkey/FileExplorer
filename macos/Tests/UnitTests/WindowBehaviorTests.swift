import XCTest
import AppKit
import ExplorerAdapters
import ExplorerDomain
@testable import MorinExplorer

/// Drives the real windows in process: the same controllers, views and commands the menus and the
/// mouse reach, without needing the UI-automation permission that XCUITest does.
final class WindowBehaviorTests: XCTestCase {
    private var root: URL!
    private var folder: URL!
    private var services: AppServices!
    private var manager: WindowManager!

    override func setUp() {
        _ = NSApplication.shared
        root = FileManager.default.temporaryDirectory.appendingPathComponent("window-\(UUID().uuidString)")
        folder = root.appendingPathComponent("folder")
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        services = AppServices(arguments: ["x", "-MorinStateDirectory", root.appendingPathComponent("state").path])
        manager = WindowManager(services: services)
    }

    override func tearDown() {
        manager.controllers.forEach { $0.window?.close() }
        try? FileManager.default.removeItem(at: root)
    }

    private func wait(_ condition: @escaping () -> Bool, timeout: TimeInterval = 6, file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline { RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.01)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }

    private func write(_ name: String, in directory: URL? = nil) {
        try? "x".write(to: (directory ?? folder).appendingPathComponent(name), atomically: true, encoding: .utf8)
    }

    private func exists(_ name: String, in directory: URL? = nil) -> Bool {
        FileManager.default.fileExists(atPath: (directory ?? folder).appendingPathComponent(name).path)
    }

    private func openWindow() -> ExplorerWindowController {
        let controller = manager.open(Location(localPath: folder.path))
        wait { controller.tab.state.directory != nil && !controller.tab.isLoading }
        settle(0.15)   // the listing is coalesced before it is shown
        return controller
    }

    private func allViews(in view: NSView) -> [NSView] { [view] + view.subviews.flatMap(allViews(in:)) }

    private func settle(_ seconds: TimeInterval = 0.2) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }

    // MARK: tabs

    func testNewTabJoinsTheWindowsTabGroup() {
        let first = openWindow()
        first.newTab(nil)
        wait { self.manager.controllers.count == 2 }
        XCTAssertEqual(first.window?.tabbedWindows?.count, 2)
        XCTAssertEqual(manager.locations.count, 2)
    }

    func testClosingATabForgetsItsController() {
        let first = openWindow()
        first.newTab(nil)
        wait { self.manager.controllers.count == 2 }
        manager.controllers.last?.window?.close()
        wait { self.manager.controllers.count == 1 }
    }

    func testEachTabKeepsItsOwnLocationAndHistory() throws {
        let child = folder.appendingPathComponent("child")
        try FileManager.default.createDirectory(at: child, withIntermediateDirectories: true)
        let first = openWindow()
        first.tab.navigate(to: Location(localPath: child.path))
        wait { first.tab.location == Location(localPath: child.path) && first.tab.state.directory != nil }
        first.newTab(nil)
        wait { self.manager.controllers.count == 2 }
        let second = manager.controllers[1]
        wait { second.tab.state.directory != nil }
        XCTAssertNotEqual(first.tab.location, second.tab.location)
        XCTAssertTrue(first.tab.canGoBack)
        XCTAssertFalse(second.tab.canGoBack)
    }

    // MARK: view modes

    func testSwitchingViewModesSwapsTheListAndTheGrid() {
        write("a.txt")
        let controller = openWindow()
        controller.tab.setOptions(ViewOptions(), forFolder: false)
        controller.tab.updateOptions { $0.viewMode = .detailedList }
        settle()
        XCTAssertTrue(allViews(in: controller.window!.contentView!).contains { $0 is NSTableView })
        controller.tab.updateOptions { $0.viewMode = .iconGrid }
        settle()
        XCTAssertTrue(allViews(in: controller.window!.contentView!).contains { $0 is NSCollectionView })
        controller.tab.updateOptions { $0.viewMode = .compactList }
        settle()
        XCTAssertTrue(allViews(in: controller.window!.contentView!).contains { $0 is NSTableView })
    }

    func testHiddenFilesToggleFromTheMenuAction() {
        write(".secret")
        write("shown.txt")
        let controller = openWindow()
        XCTAssertEqual(controller.tab.displayed.map(\.name), ["shown.txt"])
        controller.toggleHidden(nil)
        XCTAssertEqual(controller.tab.displayed.map(\.name), [".secret", "shown.txt"])
    }

    func testListColumnsFollowTheSavedLayout() {
        write("a.txt")
        let controller = openWindow()
        controller.tab.updateOptions {
            $0.viewMode = .detailedList
            $0.listColumns = ListColumnLayout(order: [.size, .name, .type, .dateModified], widths: [.size: 120])
        }
        settle()
        let table = allViews(in: controller.window!.contentView!).compactMap { $0 as? NSTableView }
            .first { $0.tableColumns.contains { $0.identifier.rawValue == "name" } }
        XCTAssertEqual(table?.tableColumns.map(\.identifier.rawValue), ["size", "name", "type", "date-modified"])
        XCTAssertEqual(table?.tableColumns.first?.width, 120)
    }

    // MARK: files

    func testDroppingOnAFolderMovesTheFileIntoIt() throws {
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("target"), withIntermediateDirectories: true)
        write("moved.txt")
        let controller = openWindow()
        let accepted = controller.browser.content.fileViewDrop(sources: [Location(localPath: folder.appendingPathComponent("moved.txt").path)],
                                                              onto: Location(localPath: folder.appendingPathComponent("target").path), copy: false)
        XCTAssertTrue(accepted)
        wait { self.exists("moved.txt", in: self.folder.appendingPathComponent("target")) }
        XCTAssertFalse(exists("moved.txt"))
    }

    func testDroppingWithCopyKeepsTheOriginal() throws {
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("target"), withIntermediateDirectories: true)
        write("kept.txt")
        let controller = openWindow()
        _ = controller.browser.content.fileViewDrop(sources: [Location(localPath: folder.appendingPathComponent("kept.txt").path)],
                                                    onto: Location(localPath: folder.appendingPathComponent("target").path), copy: true)
        wait { self.exists("kept.txt", in: self.folder.appendingPathComponent("target")) }
        XCTAssertTrue(exists("kept.txt"))
    }

    func testAFolderCannotBeDroppedIntoItself() throws {
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("self"), withIntermediateDirectories: true)
        let controller = openWindow()
        let location = Location(localPath: folder.appendingPathComponent("self").path)
        XCTAssertFalse(controller.browser.content.fileViewCanDrop(sources: [location], onto: location))
    }

    func testCopyAndPasteMakesACopyBesideTheOriginal() throws {
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("target"), withIntermediateDirectories: true)
        write("source.txt")
        let controller = openWindow()
        controller.tab.setSelection(controller.tab.displayed.filter { $0.name == "source.txt" })
        controller.copy(nil)
        controller.commands.paste(into: Location(localPath: folder.appendingPathComponent("target").path))
        wait { self.exists("source.txt", in: self.folder.appendingPathComponent("target")) }
        XCTAssertTrue(exists("source.txt"))
    }

    func testCutAndPasteMovesTheFile() throws {
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("target"), withIntermediateDirectories: true)
        write("cut.txt")
        let controller = openWindow()
        controller.tab.setSelection(controller.tab.displayed.filter { $0.name == "cut.txt" })
        controller.cut(nil)
        controller.commands.paste(into: Location(localPath: folder.appendingPathComponent("target").path))
        wait { self.exists("cut.txt", in: self.folder.appendingPathComponent("target")) }
        XCTAssertFalse(exists("cut.txt"))
        XCTAssertNil(services.clipboard.payload, "a finished cut empties the clipboard")
    }

    func testRenameFromTheViewRenamesOnDisk() {
        write("old.txt")
        let controller = openWindow()
        let item = controller.tab.displayed[0]
        controller.browser.content.fileViewRename(item, to: "new.txt")
        wait { self.exists("new.txt") }
        XCTAssertFalse(exists("old.txt"))
    }

    func testNewFolderCreatesAnUntitledFolderThenNumbersTheNext() {
        let controller = openWindow()
        controller.newFolder(nil)
        wait { self.exists("untitled folder") }
        controller.commands.newFolder()
        wait { self.exists("untitled folder 2") }
    }

    func testAConflictIsResolvedByTheRememberedChoice() throws {
        try services.preferences.update { $0.conflictDefault = .keepBoth }
        try FileManager.default.createDirectory(at: folder.appendingPathComponent("target"), withIntermediateDirectories: true)
        write("dup.txt")
        write("dup.txt", in: folder.appendingPathComponent("target"))
        let controller = openWindow()
        _ = controller.commands.transfer([Location(localPath: folder.appendingPathComponent("dup.txt").path)],
                                         to: Location(localPath: folder.appendingPathComponent("target").path), copy: true)
        wait { self.exists("dup (copy).txt", in: self.folder.appendingPathComponent("target")) || self.exists("dup (2).txt", in: self.folder.appendingPathComponent("target"))
            || ((try? FileManager.default.contentsOfDirectory(atPath: self.folder.appendingPathComponent("target").path))?.count ?? 0) == 2 }
        XCTAssertEqual((try FileManager.default.contentsOfDirectory(atPath: folder.appendingPathComponent("target").path)).count, 2)
    }

    // MARK: location bar, sidebar, properties, preferences

    func testTypingAPathIntoTheLocationBarNavigates() throws {
        let other = root.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        let controller = openWindow()
        controller.browser.locationBar.onSubmit(other.path)
        wait { controller.tab.location == Location(localPath: other.path) }
    }

    func testAnInvalidLocationIsRejectedWithoutNavigating() {
        let controller = openWindow()
        let before = controller.tab.location
        controller.browser.locationBar.onSubmit("not a path")
        settle(0.4)
        XCTAssertEqual(controller.tab.location, before)
    }

    func testBreadcrumbReflectsTheLocation() {
        let controller = openWindow()
        let buttons = allViews(in: controller.browser.locationBar).compactMap { $0 as? NSButton }.map(\.title)
        XCTAssertEqual(buttons.last, "folder")
    }

    func testSidebarListsQuickAccessAndThisMac() {
        let controller = openWindow()
        settle()
        let labels = controller.sidebar.visibleLabels
        XCTAssertTrue(labels.contains("Quick access"))
        XCTAssertTrue(labels.contains("This Mac"))
        XCTAssertTrue(labels.contains("Macintosh HD"))
    }

    func testStatusBarCountsItemsAndSelection() {
        write("a.txt")
        write("b.txt")
        let controller = openWindow()
        XCTAssertEqual(controller.tab.statusText, "2 items")
        controller.tab.setSelection([controller.tab.displayed[0]])
        XCTAssertTrue(controller.tab.statusText.contains("1 selected"))
    }

    func testPropertiesWindowShowsTheFilesDetails() {
        write("info.txt")
        let controller = openWindow()
        let properties = PropertiesWindowController(item: controller.tab.displayed[0], services: services)
        properties.showWindow(nil)
        let texts = allViews(in: properties.window!.contentView!).compactMap { ($0 as? NSTextField)?.stringValue }
        XCTAssertTrue(texts.contains { $0.contains("1 byte") || $0.contains("1 B") || $0.contains("(1 bytes)") }, "\(texts)")
        XCTAssertEqual(properties.window?.title, "info.txt Info")
        properties.close()
    }

    func testPropertiesPermissionChangeIsApplied() throws {
        write("perm.txt")
        let path = folder.appendingPathComponent("perm.txt").path
        var bits = PermissionBits()
        bits.ownerRead = true; bits.ownerWrite = true; bits.groupRead = true
        try PropertyLoader.setPermissions(bits, at: path)
        XCTAssertEqual(try PropertyLoader.load(path).permissions, bits)
    }

    func testPreferencesPersistAcrossAServicesRestart() {
        let model = PreferencesModel(services: services)
        model.update { $0.restorePreviousTabs = true; $0.operationHistoryDays = 3 }
        let again = AppServices(arguments: ["x", "-MorinStateDirectory", root.appendingPathComponent("state").path])
        XCTAssertTrue(again.preferences.preferences.restorePreviousTabs)
        XCTAssertEqual(again.preferences.preferences.operationHistoryDays, 3)
    }

    func testSessionRestoreRoundTripsOpenTabs() throws {
        let first = openWindow()
        first.newTab(nil)
        wait { self.manager.controllers.count == 2 }
        wait { self.manager.controllers[1].tab.state.directory != nil }
        try services.preferences.update { $0.restorePreviousTabs = true }
        try services.preferences.saveSession(manager.locations)
        let restored = try services.preferences.loadSession().locations
        XCTAssertEqual(restored.count, 2)
    }

    // MARK: search

    func testSearchFromTheToolbarShowsMatchesWithRelativePaths() throws {
        let nested = folder.appendingPathComponent("deep")
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        write("needle.txt", in: nested)
        write("hay.txt")
        let controller = openWindow()
        controller.tab.beginSearch("needle", mode: .filename)
        wait { controller.tab.displayed.map(\.name) == ["needle.txt"] }
        XCTAssertEqual(controller.tab.displayName(for: controller.tab.displayed[0]), "deep/needle.txt")
    }

    func testContentSearchIsRefusedUntilItIsTurnedOn() {
        let controller = openWindow()
        let item = NSMenuItem(title: "File Contents", action: NSSelectorFromString("chooseSearchMode:"), keyEquivalent: "")
        item.tag = 1
        XCTAssertFalse(controller.validateMenuItem(item))
    }

    // MARK: archives

    private func makeArchive() throws -> URL {
        let source = root.appendingPathComponent("payload")
        try FileManager.default.createDirectory(at: source.appendingPathComponent("inner"), withIntermediateDirectories: true)
        try "a".write(to: source.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        try "b".write(to: source.appendingPathComponent("inner/b.txt"), atomically: true, encoding: .utf8)
        let path = try ArchiveTools.compressBlocking(sources: [source.path], format: .zip, progress: { _, _ in }, cancelled: { false })
        let target = folder.appendingPathComponent("bundle.zip")
        try FileManager.default.moveItem(atPath: path, toPath: target.path)
        return target
    }

    func testOpeningAnArchiveBrowsesItLikeAFolder() throws {
        _ = try makeArchive()
        let controller = openWindow()
        let archive = try XCTUnwrap(controller.tab.displayed.first { $0.name == "bundle.zip" })
        controller.tab.open(archive)
        wait { controller.tab.location.isInsideArchive && controller.tab.displayed.map(\.name) == ["payload"] }
        controller.tab.open(controller.tab.displayed[0])
        wait { controller.tab.displayed.map(\.name).contains("a.txt") }
        XCTAssertTrue(controller.tab.displayed.map(\.name).contains("inner"))
    }

    func testExtractHereUnpacksNextToTheArchive() throws {
        _ = try makeArchive()
        let controller = openWindow()
        controller.tab.setSelection(controller.tab.displayed.filter { $0.name == "bundle.zip" })
        controller.extractHere(nil)
        wait { self.exists("a.txt", in: self.folder.appendingPathComponent("bundle")) || self.exists("a.txt", in: self.folder.appendingPathComponent("payload")) || self.exists("payload", in: self.folder.appendingPathComponent("bundle")) }
    }

    func testCompressCreatesAnArchiveBesideTheSelection() {
        write("pack.txt")
        let controller = openWindow()
        controller.tab.setSelection(controller.tab.displayed.filter { $0.name == "pack.txt" })
        controller.compressZip(nil)
        wait { ((try? FileManager.default.contentsOfDirectory(atPath: self.folder.path)) ?? []).contains { $0.hasPrefix("pack") && $0.hasSuffix(".zip") } }
    }

    // MARK: search history

    func testSubmittedSearchesAreRememberedOnlyWhileThePrivacySettingAllowsIt() throws {
        let controller = openWindow()
        controller.tab.recordSearchTerm("budget")
        XCTAssertEqual(services.searchHistory.entries.map(\.term), ["budget"])
        try services.preferences.update { $0.recordSearchHistory = false }
        controller.tab.recordSearchTerm("secret")
        XCTAssertTrue(services.searchHistory.entries.isEmpty, "turning history off forgets what was kept")
    }

    // MARK: watching

    func testAFileCreatedOutsideTheAppAppearsWithoutARefresh() {
        write("first.txt")
        let controller = openWindow()
        XCTAssertEqual(controller.tab.displayed.map(\.name), ["first.txt"])
        write("second.txt")
        wait({ controller.tab.displayed.map(\.name) == ["first.txt", "second.txt"] }, timeout: 10)
    }

    func testAFileChangedOutsideTheAppIsShownWithItsNewSize() throws {
        write("grow.txt")
        let controller = openWindow()
        func size() -> UInt64? {
            controller.tab.requestMetadata(for: controller.tab.displayed)
            return controller.tab.displayed.first { $0.name == "grow.txt" }?.size
        }
        wait { size() == 1 }
        let handle = try FileHandle(forWritingTo: folder.appendingPathComponent("grow.txt"))
        try handle.seekToEnd()
        try handle.write(contentsOf: Data(repeating: 0x78, count: 1000))
        try handle.close()
        wait({ size() == 1001 }, timeout: 10)
    }

    func testTheDirectoryWatcherReportsCreationModificationAndRemoval() throws {
        write("w.txt")
        let controller = openWindow()
        var fired = 0
        let handle = controller.tab.navigation.watch(Location(localPath: folder.path)) { fired += 1 }
        XCTAssertNotNil(handle)
        settle(0.5)
        let before = fired
        let file = try FileHandle(forWritingTo: folder.appendingPathComponent("w.txt"))
        try file.seekToEnd()
        try file.write(contentsOf: Data("more".utf8))
        try file.close()
        wait({ fired > before }, timeout: 8)
        let afterModify = fired
        try FileManager.default.removeItem(at: folder.appendingPathComponent("w.txt"))
        wait({ fired > afterModify }, timeout: 8)
        handle?.cancel()
    }

    func testNavigatingAwayForgetsTheFetchedMetadata() throws {
        write("meta.txt")
        let other = root.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        let controller = openWindow()
        controller.tab.requestMetadata(for: controller.tab.displayed)
        wait { controller.tab.fetchedMetadataCount == 1 }
        controller.tab.navigate(to: Location(localPath: other.path))
        XCTAssertEqual(controller.tab.fetchedMetadataCount, 0, "metadata fetched for a folder that was left is dropped at once")
    }
}
