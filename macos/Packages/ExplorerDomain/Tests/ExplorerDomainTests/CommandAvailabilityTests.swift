import XCTest
@testable import ExplorerDomain

final class CommandAvailabilityTests: XCTestCase {
    private func item(_ name: String, uri: String? = nil, directory: Bool = false, caps: Set<Capability>, type: String? = nil) -> FileItem {
        let location = uri.map { try! Location(uri: $0) } ?? Location(localPath: "/tmp/\(name)")
        return FileItem(snapshot: EntrySnapshot(location: location, displayName: name, kind: directory ? .directory : .file, contentType: type,
                                                capabilities: CapabilitySnapshot(supported: caps)))
    }

    private let everything: Set<Capability> = [.open, .copy, .move, .rename, .trash, .delete, .create]

    func testWriteAccessDoesNotImplyRename() {
        let item = item("file", caps: [.open, .copy, .create])
        let availability = CommandAvailability(selection: [item], destinationCapabilities: nil)
        XCTAssertFalse(availability.canCut)
        XCTAssertFalse(availability.canRename)
    }

    func testRemoteItemsKeepIdentityAndDisableTerminalAndLocalActions() {
        let item = item("report.txt", uri: "smb://host/share/report.txt", caps: [.open, .copy])
        let availability = CommandAvailability(selection: [item], destination: nil)
        XCTAssertFalse(availability.canTrash)
        XCTAssertEqual(availability.disabledReason, "This location does not support Trash")
        XCTAssertFalse(availability.allows(.openInTerminal))
        XCTAssertFalse(availability.canCompress)
    }

    func testProviderCapabilitiesEnableExecution() {
        let item = item("report.txt", uri: "smb://host/share/report.txt", caps: everything)
        let availability = CommandAvailability(selection: [item], destination: nil)
        XCTAssertTrue(availability.canOpen && availability.canCopy && availability.canCut && availability.canRename && availability.canTrash && availability.canDelete)
    }

    func testAMixedSelectionNeedsEveryItemToSupportTheCommand() {
        let items = [item("a", caps: [.open, .copy, .move]), item("b", caps: [.open, .copy])]
        let availability = CommandAvailability(selection: items, destinationCapabilities: nil)
        XCTAssertTrue(availability.canCopy)
        XCTAssertFalse(availability.canCut)
        XCTAssertFalse(availability.canRename)
    }

    func testPasteNeedsADestinationThatCanCreate() {
        XCTAssertTrue(CommandAvailability(selection: [], destination: Location(localPath: "/tmp")).canPaste)
        XCTAssertFalse(CommandAvailability(selection: [], destination: try! Location(uri: "smb://h/s")).canPaste)
    }

    func testExtractIsOfferedOnlyForArchives() {
        let archive = item("holiday.zip", caps: everything, type: "public.zip-archive")
        let text = item("notes.txt", caps: everything, type: "public.plain-text")
        XCTAssertTrue(CommandAvailability(selection: [archive], destination: nil).canExtract)
        XCTAssertFalse(CommandAvailability(selection: [archive, text], destination: nil).canExtract)
    }

    func testTerminalIsForASingleLocalFolder() {
        let folder = item("dir", directory: true, caps: everything)
        XCTAssertTrue(CommandAvailability(selection: [folder], destination: nil).canOpenTerminal)
        XCTAssertFalse(CommandAvailability(selection: [folder, folder], destination: nil).canOpenTerminal)
    }

    func testShiftChangesTheDeleteCommand() {
        XCTAssertEqual(CommandAvailability.deleteCommand(shiftHeld: true), .deletePermanently)
        XCTAssertEqual(CommandAvailability.deleteCommand(shiftHeld: false), .moveToTrash)
    }

    func testShortcutsRouteToTheSameCommandsAsTheMenu() {
        XCTAssertEqual(Shortcut.paste.command, .paste)
        XCTAssertEqual(Shortcut.trash.command, .moveToTrash)
    }

    func testContextMenusOfferTheRightCommands() {
        let archive = item("a.zip", caps: everything, type: "public.zip-archive")
        XCTAssertTrue(CommandAvailability.itemCommands(for: .file(archive)).contains(.extract))
        XCTAssertFalse(CommandAvailability.itemCommands(for: .file(item("a.txt", caps: everything, type: "public.plain-text"))).contains(.extract))
        XCTAssertTrue(CommandAvailability.itemCommands(for: .folder(item("d", directory: true, caps: everything))).contains(.openInTerminal))
        XCTAssertEqual(CommandAvailability.itemCommands(for: .tab), [.closeTab, .closeOtherTabs, .duplicateTab])
    }

    func testPermissionBitsRoundTrip() {
        XCTAssertEqual(PermissionBits(mode: 0o754).mode, 0o754)
        XCTAssertTrue(PermissionBits(mode: 0o644).ownerWrite)
        XCTAssertFalse(PermissionBits(mode: 0o644).groupWrite)
    }

    func testByteFormat() {
        XCTAssertEqual(ByteFormat.string(512), "512 B")
        XCTAssertEqual(ByteFormat.string(1536), "1.5 KiB")
    }
}
