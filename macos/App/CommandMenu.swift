import AppKit
import ExplorerDomain

final class CommandMenuItem: NSMenuItem {
    var command: ContextCommand = .open
    var items: [FileItem] = []
}

enum CommandTitles {
    static func title(for command: ContextCommand, items: [FileItem], pinned: Bool = false, shiftHeld: Bool = false) -> String {
        switch command {
        case .open: return "Open"
        case .openWith: return "Open With"
        case .extract: return "Extract Here"
        case .compressZip: return "Compress to .zip"
        case .compress7z: return "Compress to .7z"
        case .compressTarGz: return "Compress to .tar.gz"
        case .openInNewTab: return "Open in New Tab"
        case .openInNewWindow: return "Open in New Window"
        case .openInTerminal: return "Open in Terminal"
        case .newFolder: return "New Folder"
        case .cut: return "Cut"
        case .copy: return "Copy"
        case .copyFilename: return "Copy Name"
        case .copyFullPath: return "Copy Path"
        case .copyTo: return "Copy To…"
        case .moveTo: return "Move To…"
        case .paste: return "Paste"
        case .rename: return "Rename…"
        case .moveToTrash: return "Move to Trash"
        case .deletePermanently: return "Delete Immediately…"
        case .properties: return "Get Info"
        case .refresh: return "Refresh"
        case .closeTab: return "Close Tab"
        case .closeOtherTabs: return "Close Other Tabs"
        case .duplicateTab: return "Duplicate Tab"
        case .toggleQuickAccess: return pinned ? "Remove from Quick Access" : "Add to Quick Access"
        }
    }
}

enum CommandMenuBuilder {
    /// The context menu for a selection (or for the folder background when nothing is selected).
    static func menu(items: [FileItem], tab: TabModel, services: AppServices, commands: FileCommands, target: AnyObject, action: Selector) -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        let list: [ContextCommand]
        let availability: CommandAvailability
        let background = items.isEmpty
        if background {
            availability = CommandAvailability(selection: [], destinationCapabilities: tab.isSearching ? nil : tab.capabilities)
            list = [.newFolder, .paste, .refresh, .openInTerminal, .toggleQuickAccess]
        } else {
            availability = CommandAvailability(selection: items, destinationCapabilities: nil)
            let first = items[0]
            let single = CommandAvailability.itemCommands(for: first.isDirectory ? .folder(first) : .file(first))
            list = items.count == 1 ? single : single.filter { ![.open, .openWith, .openInNewTab, .openInNewWindow, .openInTerminal, .rename, .toggleQuickAccess, .extract].contains($0) || $0 == .open }
        }
        var previous: ContextCommand?
        let groups: [[ContextCommand]] = [[.open, .openWith, .openInNewTab, .openInNewWindow, .openInTerminal, .extract],
                                          [.compressZip, .compress7z, .compressTarGz],
                                          [.cut, .copy, .paste, .copyFilename, .copyFullPath, .copyTo, .moveTo],
                                          [.newFolder, .rename, .moveToTrash, .deletePermanently],
                                          [.properties, .refresh, .toggleQuickAccess]]
        func group(of command: ContextCommand) -> Int { groups.firstIndex { $0.contains(command) } ?? 99 }
        let ordered = list.sorted { (group(of: $0), groups[safe: group(of: $0)]?.firstIndex(of: $0) ?? 0) < (group(of: $1), groups[safe: group(of: $1)]?.firstIndex(of: $1) ?? 0) }
        for command in ordered {
            if let previous, group(of: previous) != group(of: command) { menu.addItem(.separator()) }
            previous = command
            let pinned = items.first.map { services.sidebarPreferences.isPinned($0.location) } ?? services.sidebarPreferences.isPinned(tab.location)
            let entry = CommandMenuItem(title: CommandTitles.title(for: command, items: items, pinned: pinned), action: nil, keyEquivalent: "")
            entry.command = command
            entry.items = items
            var enabled = availability.allows(command)
            if command == .paste { enabled = enabled && commands.canPaste }
            if command == .openInTerminal && background { enabled = tab.location.localPath != nil }
            if command == .openWith {
                entry.submenu = commands.openWithMenu(for: items, target: target)
                enabled = enabled && items.count == 1 && !items[0].isDirectory && items[0].location.localPath != nil
            }
            if enabled { entry.action = action; entry.target = target }
            else { entry.toolTip = availability.disabledReason(for: command) }
            menu.addItem(entry)
        }
        return menu
    }
}

extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
