import XCTest
@testable import ExplorerDomain

private final class FakeVolumes: VolumeProvider {
    var mounted: [MountedVolume] = []
    var unmounted: [MountableVolume] = []
    func mountedVolumes() -> [MountedVolume] { mounted }
    func unmountedVolumes() -> [MountableVolume] { unmounted }
}

final class SidebarTests: XCTestCase {
    private func model(volumes: VolumeProvider = NoVolumes(), preferences: SidebarPreferences = .inMemory(), name: String = "Ada Lovelace") -> SidebarModel {
        SidebarModel(preferences: preferences, home: "/Users/ada", userName: name, volumes: volumes, exists: { _ in true })
    }

    func testSidebarSeparatesPersonalFoldersFromDevices() {
        let sections = model().sections()
        XCTAssertEqual(sections.map(\.title), ["Quick access", "Ada Lovelace", "This Mac"])
        XCTAssertEqual(sections[1].nodes.first?.location.label, "Home")
        XCTAssertEqual(sections[2].nodes.first?.location.label, "Macintosh HD")
    }

    func testUserSectionPrefersTheAccountNameAndFallsBackToTheHomeFolder() {
        XCTAssertEqual(model(name: "Ada Lovelace").sections()[1].title, "Ada Lovelace")
        XCTAssertEqual(model(name: "  ").sections()[1].title, "ada")
        XCTAssertEqual(model(name: "unknown").sections()[1].title, "ada")
    }

    func testRemovableMediaIsListedBeforeFixedDrives() {
        let volumes = FakeVolumes()
        volumes.mounted = [MountedVolume(name: "Backup", path: "/Volumes/Backup", kind: .internalDisk),
                           MountedVolume(name: "Stick", path: "/Volumes/Stick", kind: .removable)]
        XCTAssertEqual(model(volumes: volumes).sections()[2].nodes.map(\.location.label), ["Macintosh HD", "Stick", "Backup"])
    }

    func testVolumesInsideHomeAndTheRootAreNotRepeated() {
        let volumes = FakeVolumes()
        volumes.mounted = [MountedVolume(name: "Root", path: "/", kind: .internalDisk), MountedVolume(name: "Inside", path: "/Users/ada/mnt", kind: .other)]
        XCTAssertEqual(model(volumes: volumes).sections()[2].nodes.count, 1)
    }

    func testUnmountedVolumesAreKeptApartBecauseTheyHaveNoLocation() {
        let volumes = FakeVolumes()
        volumes.unmounted = [MountableVolume(id: "uuid-1", label: "Camera", iconName: "externaldrive")]
        let section = model(volumes: volumes).sections()[2]
        XCTAssertEqual(section.mountables.map(\.id), ["uuid-1"])
        XCTAssertEqual(section.nodes.count, 1)
    }

    func testFirstRunPinsTheHomeFolders() {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("sidebar-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let preferences = SidebarPreferences(file: file)
        _ = model(preferences: preferences)
        XCTAssertEqual(preferences.pins.map(\.uri), ["/Users/ada", "/Users/ada/Desktop", "/Users/ada/Documents", "/Users/ada/Downloads", "/Users/ada/Pictures"].map { Location(localPath: $0).uri })
    }

    func testPinningAPathIsPersistedOnce() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("sidebar-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let preferences = SidebarPreferences(file: file)
        let place = Location(localPath: "/Volumes/Work")
        preferences.pin(place); preferences.pin(place)
        try preferences.save()
        XCTAssertEqual(SidebarPreferences(file: file).pins, [place])
    }

    func testUnpinningRemovesTheMatchingLocation() {
        let preferences = SidebarPreferences.inMemory()
        let place = Location(localPath: "/tmp/a")
        preferences.pin(place); preferences.unpin(place)
        XCTAssertTrue(preferences.pins.isEmpty)
    }

    func testSectionAndOccurrenceStateRoundTrip() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("sidebar-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let preferences = SidebarPreferences(file: file)
        preferences.setSectionExpanded("This Mac", false)
        preferences.setOccurrenceExpanded(["Quick access", "file:///tmp"], true)
        try preferences.save()
        let reloaded = SidebarPreferences(file: file)
        XCTAssertFalse(reloaded.isSectionExpanded("This Mac"))
        XCTAssertTrue(reloaded.isOccurrenceExpanded(["Quick access", "file:///tmp"]))
        XCTAssertFalse(reloaded.isOccurrenceExpanded(["Quick access", "file:///elsewhere"]))
    }

    func testDevicesGroupDefaultsToCollapsedAndPersists() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("sidebar-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let preferences = SidebarPreferences(file: file)
        XCTAssertFalse(preferences.devicesExpanded)
        preferences.devicesExpanded = true
        try preferences.save()
        XCTAssertTrue(SidebarPreferences(file: file).devicesExpanded)
    }

    func testBadPinRecordsAreSkippedWithANotice() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("sidebar-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        try #"{"schema_version":2,"locations":["file:///ok","smb://user:pw@host/x",7]}"#.write(to: file, atomically: true, encoding: .utf8)
        let preferences = SidebarPreferences(file: file)
        XCTAssertEqual(preferences.pins.map(\.uri), ["file:///ok"])
        XCTAssertEqual(preferences.notices, [.malformedRecord, .malformedRecord])
    }

    func testChildrenFromASnapshotAreFoldersSortedByName() {
        let root = Location(localPath: "/tmp/x")
        let entries = [("b", EntryKind.directory), ("a", .directory), ("file.txt", .file)].map {
            EntrySnapshot(location: Location(localPath: "/tmp/x/\($0.0)"), displayName: $0.0, kind: $0.1)
        }
        let snapshot = DirectorySnapshot(location: ResolvedLocation(requested: root, resolved: root), entries: entries)
        XCTAssertEqual(SidebarModel.children(from: snapshot).map(\.location.label), ["a", "b"])
    }
}
