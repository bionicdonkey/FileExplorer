import Foundation

public enum ContextCommand: Equatable, CaseIterable {
    case open, openWith, extract, compressZip, compress7z, compressTarGz
    case openInNewTab, openInNewWindow, openInTerminal, newFolder
    case cut, copy, copyFilename, copyFullPath, copyTo, moveTo, paste
    case rename, moveToTrash, deletePermanently, properties, refresh
    case closeTab, closeOtherTabs, duplicateTab, toggleQuickAccess
}

/// Keyboard intent that routes to the same command as the menu and toolbar.
public enum Shortcut: Equatable {
    case cut, copy, paste, trash, deletePermanently

    public var command: ContextCommand {
        switch self {
        case .cut: return .cut
        case .copy: return .copy
        case .paste: return .paste
        case .trash: return .moveToTrash
        case .deletePermanently: return .deletePermanently
        }
    }
}

public enum ContextTarget {
    case file(FileItem)
    case folder(FileItem)
    case background(Location)
    case tab
    case sidebarLocation(Location)
}

/// What the commands may do, derived only from capabilities: a command is on when every selected
/// item supports it, so the menu, the toolbar and the keyboard can never disagree.
public struct CommandAvailability: Equatable {
    public var canOpen = false
    public var canCopy = false
    public var canCut = false
    public var canPaste = false
    public var canRename = false
    public var canUseLocalAdapter = false
    public var canOpenTerminal = false
    public var canTrash = false
    public var canDelete = false
    public var canExtract = false
    public var canCompress = false
    public var disabledReason: String?

    /// For a selection pasted or dropped into `destination` (a local location can be written to).
    public init(selection: [FileItem], destination: Location?) {
        let capabilities = destination.flatMap { $0.localPath != nil ? CapabilitySnapshot(supported: [.create]) : nil }
        self.init(selection: selection, destinationCapabilities: capabilities)
    }

    public init(selection: [FileItem], destinationCapabilities destination: CapabilitySnapshot?) {
        func supportsAll(_ capability: Capability) -> Bool { !selection.isEmpty && selection.allSatisfy { $0.capabilities.supports(capability) } }
        canPaste = destination?.supports(.create) ?? false
        canOpen = supportsAll(.open)
        canUseLocalAdapter = selection.count == 1 && selection[0].location.localPath != nil
        canCopy = supportsAll(.copy)
        canCut = supportsAll(.move)
        canRename = selection.count == 1 && supportsAll(.rename)
        canOpenTerminal = canUseLocalAdapter && selection[0].isDirectory && canOpen
        canTrash = supportsAll(.trash)
        canDelete = supportsAll(.delete)
        canExtract = !selection.isEmpty && selection.allSatisfy(isExtractable)
        canCompress = !selection.isEmpty && selection.allSatisfy { $0.location.localPath != nil }
        if !canTrash && !selection.isEmpty { disabledReason = "This location does not support Trash" }
        else if !canDelete && !selection.isEmpty { disabledReason = "This location does not support permanent deletion" }
        else if !canPaste && destination != nil { disabledReason = "This location does not support Paste" }
    }

    public init(target: ContextTarget) {
        switch target {
        case .file(let item), .folder(let item): self.init(selection: [item], destination: nil)
        case .background(let location), .sidebarLocation(let location): self.init(selection: [], destination: location)
        case .tab:
            self.init(selection: [], destination: nil)
            canOpen = true; canCopy = true; canCut = true; canPaste = true; canRename = true; canUseLocalAdapter = true
            canOpenTerminal = true; canTrash = true; canDelete = true; canExtract = true; canCompress = true
            disabledReason = nil
        }
    }

    public func allows(_ command: ContextCommand) -> Bool {
        switch command {
        case .open, .openWith, .openInNewTab, .openInNewWindow, .properties: return canOpen
        case .openInTerminal: return canOpenTerminal
        case .extract: return canExtract
        case .compressZip, .compress7z, .compressTarGz: return canCompress
        case .cut, .moveTo: return canCut
        case .copy, .copyFilename, .copyFullPath, .copyTo: return canCopy
        case .paste, .newFolder: return canPaste
        case .rename: return canRename
        case .moveToTrash: return canTrash
        case .deletePermanently: return canDelete
        case .refresh, .closeTab, .closeOtherTabs, .duplicateTab, .toggleQuickAccess: return true
        }
    }

    public func disabledReason(for command: ContextCommand) -> String? {
        if allows(command) { return nil }
        switch command {
        case .moveToTrash: return "This location does not support Trash"
        case .deletePermanently: return "This location does not support permanent deletion"
        case .paste, .newFolder: return "This location does not support Paste"
        case .openInTerminal: return "Open in Terminal is available for local folders only"
        case .compressZip, .compress7z, .compressTarGz: return "Compress is available for local files only"
        default: return "This location does not support this action"
        }
    }

    public static func deleteCommand(shiftHeld: Bool) -> ContextCommand { shiftHeld ? .deletePermanently : .moveToTrash }
    public static func deleteLabel(shiftHeld: Bool) -> String { shiftHeld ? "Delete Immediately…" : "Move to Trash" }

    /// The commands a context menu for `target` offers, in order.
    public static func itemCommands(for target: ContextTarget) -> [ContextCommand] {
        switch target {
        case .file(let item):
            var commands: [ContextCommand] = [.open, .openWith]
            if isExtractable(item) { commands.append(.extract) }
            return commands + [.compressZip, .compress7z, .compressTarGz, .cut, .copy, .copyFilename, .copyFullPath, .copyTo, .moveTo, .rename, .moveToTrash, .properties]
        case .folder, .sidebarLocation:
            return [.open, .openInNewTab, .openInNewWindow, .openInTerminal, .compressZip, .compress7z, .compressTarGz, .cut, .copy, .copyFilename,
                    .copyFullPath, .copyTo, .moveTo, .rename, .moveToTrash, .properties, .toggleQuickAccess]
        case .background: return [.newFolder, .paste, .refresh, .toggleQuickAccess]
        case .tab: return [.closeTab, .closeOtherTabs, .duplicateTab]
        }
    }
}
