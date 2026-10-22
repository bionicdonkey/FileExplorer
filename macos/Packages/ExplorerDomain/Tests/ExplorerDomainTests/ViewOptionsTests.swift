import XCTest
@testable import ExplorerDomain

final class ViewOptionsTests: XCTestCase {
    func testExplicitFolderOptionsRoundTrip() {
        let store = ViewPreferencesStore.inMemory()
        var expected = ViewOptions(); expected.viewMode = .iconGrid; expected.gridSize = .large
        store.set(expected, for: "/tmp/Pictures")
        XCTAssertEqual(store.options(for: "/tmp/Pictures"), expected)
    }

    func testPictureFolderUsesGridWhenNoOverrideExists() {
        XCTAssertEqual(ViewPreferencesStore.inMemory().options(for: "/tmp/Pictures").viewMode, .iconGrid)
    }

    func testCustomColumnOrderAndWidthsAreKept() {
        var expected = ViewOptions()
        expected.listColumns = ListColumnLayout(order: [.size, .name, .type, .dateModified], widths: [.name: 420, .size: 110])
        let store = ViewPreferencesStore.inMemory()
        store.set(expected, for: "/tmp/columns")
        XCTAssertEqual(store.options(for: "/tmp/columns"), expected)
    }

    func testMalformedColumnLayoutReturnsAllColumnsOnceInDefaultOrder() {
        XCTAssertEqual(ListColumnLayout(order: [.name, .name]).normalized().order, ListColumn.all)
    }

    func testGridSizesExpandTileGeometry() {
        XCTAssertLessThan(GridSize.small.layout.tileWidth, GridSize.medium.layout.tileWidth)
        XCTAssertLessThan(GridSize.medium.layout.tileWidth, GridSize.large.layout.tileWidth)
        XCTAssertLessThan(GridSize.large.layout.tileWidth, GridSize.extraLarge.layout.tileWidth)
        XCTAssertLessThan(GridSize.small.layout.verticalPadding, GridSize.medium.layout.verticalPadding)
    }

    func testHiddenFilesAreOffByDefaultAndPersistPerFolder() {
        XCTAssertFalse(ViewOptions().showHidden)
        var options = ViewOptions(); options.showHidden = true
        let store = ViewPreferencesStore.inMemory()
        store.set(options, for: "/tmp/hidden")
        XCTAssertTrue(store.options(for: "/tmp/hidden").showHidden)
    }

    func testGlobalDefaultsApplyUnlessAFolderOverridesThem() {
        let store = ViewPreferencesStore.inMemory()
        var grid = ViewOptions(); grid.viewMode = .iconGrid
        store.setDefaults(grid)
        XCTAssertEqual(store.options(for: "/tmp/plain").viewMode, .iconGrid)
        var list = ViewOptions(); list.viewMode = .detailedList
        store.set(list, for: "/tmp/custom")
        XCTAssertEqual(store.options(for: "/tmp/custom").viewMode, .detailedList)
    }

    func testClearingFoldersPreservesGlobalDefaults() {
        let store = ViewPreferencesStore.inMemory()
        var grid = ViewOptions(); grid.viewMode = .iconGrid
        store.setDefaults(grid)
        var list = ViewOptions(); list.viewMode = .detailedList
        store.set(list, for: "/tmp/first"); store.set(list, for: "/tmp/second")
        XCTAssertEqual(store.clearAllFolders(), 2)
        XCTAssertEqual(store.options(for: "/tmp/first").viewMode, .iconGrid)
    }

    func testStoreSurvivesAReload() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("view-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let store = ViewPreferencesStore(file: file)
        var options = ViewOptions(); options.sortField = .size; options.sortDirection = .descending
        store.set(options, for: "/tmp/reload")
        try store.save()
        XCTAssertEqual(ViewPreferencesStore(file: file).options(for: "/tmp/reload").sortField, .size)
    }
}

final class DirectoryModelTests: XCTestCase {
    private func item(_ name: String, directory: Bool = false, size: UInt64 = 0, modified: Date = Date(), type: String? = nil) -> FileItem {
        FileItem(snapshot: EntrySnapshot(location: Location(localPath: "/tmp/\(name)"), displayName: name, kind: directory ? .directory : .file,
                                         size: size, modified: modified, contentType: type))
    }

    func testFoldersFirstKeepsFoldersBeforeLargerFiles() {
        var options = ViewOptions(); options.sortField = .size; options.foldersFirst = true
        let sorted = DirectoryModel.sorted([item("large-file", size: 4096), item("folder", directory: true)], options: options)
        XCTAssertTrue(sorted[0].isDirectory)
    }

    func testNameSortIsCaseInsensitiveThenExact() {
        let sorted = DirectoryModel.sorted([item("b"), item("B"), item("a")], options: ViewOptions())
        XCTAssertEqual(sorted.map(\.name), ["a", "B", "b"])
    }

    func testDescendingReverses() {
        var options = ViewOptions(); options.sortDirection = .descending; options.foldersFirst = false
        XCTAssertEqual(DirectoryModel.sorted([item("a"), item("b")], options: options).map(\.name), ["b", "a"])
    }

    func testHiddenFilesAreFilteredUnlessShown() {
        var options = ViewOptions()
        XCTAssertEqual(DirectoryModel.visible([item(".secret"), item("open")], options: options).map(\.name), ["open"])
        options.showHidden = true
        XCTAssertEqual(DirectoryModel.visible([item(".secret"), item("open")], options: options).count, 2)
    }

    func testDateGroupUsesTodayForAnItemModifiedToday() {
        XCTAssertEqual(DirectoryModel.groupLabel(for: item("today.txt"), field: .dateModified, typeName: { _ in "" }), "Today")
        let old = item("old", modified: Date(timeIntervalSinceNow: -400 * 86_400))
        XCTAssertEqual(DirectoryModel.groupLabel(for: old, field: .dateModified, typeName: { _ in "" }), "Older")
    }

    func testSizeGroups() {
        XCTAssertEqual(DirectoryModel.groupLabel(for: item("e"), field: .size, typeName: { _ in "" }), "Empty")
        XCTAssertEqual(DirectoryModel.groupLabel(for: item("s", size: 10), field: .size, typeName: { _ in "" }), "Small")
        XCTAssertEqual(DirectoryModel.groupLabel(for: item("v", size: 2_000_000_000), field: .size, typeName: { _ in "" }), "Very large")
    }

    func testGroupingKeepsFirstSeenOrder() {
        var options = ViewOptions(); options.groupField = .type
        let groups = DirectoryModel.grouped([item("a", directory: true), item("b.txt"), item("c", directory: true)], options: options, typeName: { _ in "Text" })
        XCTAssertEqual(groups.map(\.title), ["Folders", "Text"])
        XCTAssertEqual(groups[0].items.count, 2)
    }
}
