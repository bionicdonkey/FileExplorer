import AppKit
import ExplorerAdapters
import ExplorerDomain

/// The file commands behind every menu, toolbar button, shortcut and drop. Each one checks
/// `CommandAvailability`, then submits to the one operation manager.
final class FileCommands {
    private let services: AppServices
    private let currentTab: () -> TabModel?
    private let window: () -> NSWindow?
    var openInNewTab: (Location) -> Void = { _ in }
    var openInNewWindow: (Location) -> Void = { _ in }
    var showProperties: (FileItem) -> Void = { _ in }

    init(services: AppServices, tab: @escaping () -> TabModel?, window: @escaping () -> NSWindow?) {
        self.services = services
        currentTab = tab
        self.window = window
    }

    private var operations: OperationManager { services.operations }

    // MARK: clipboard

    func copy(_ items: [FileItem], cut: Bool) {
        let availability = CommandAvailability(selection: items, destinationCapabilities: nil)
        guard !items.isEmpty, cut ? availability.canCut : availability.canCopy else { return NSSound.beep() }
        services.clipboard.replace(ClipboardPayload(action: cut ? .cut : .copy, locations: items.map(\.location)))
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        let urls = items.compactMap(\.pasteboardURL)
        if !urls.isEmpty { pasteboard.writeObjects(urls) }
        pasteboard.setString(items.map(\.name).joined(separator: "\n"), forType: .string)
        services.clipboardChangeCount = pasteboard.changeCount
    }

    /// The pasteboard's sources: our own clipboard when it is still the current one (it knows about cut
    /// and about items inside archives), otherwise whatever file URLs another application put there.
    private func pasteSources() -> (locations: [Location], action: ClipboardAction)? {
        let pasteboard = NSPasteboard.general
        if let payload = services.clipboard.payload, services.clipboardChangeCount == pasteboard.changeCount {
            return (payload.locations, payload.action)
        }
        let urls = (pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
        return urls.isEmpty ? nil : (urls.map { Location(localPath: $0.path) }, .copy)
    }

    var canPaste: Bool {
        guard let tab = currentTab(), !tab.isSearching else { return false }
        return CommandAvailability(selection: [], destinationCapabilities: tab.capabilities).canPaste && pasteSources() != nil
    }

    func paste(into destination: Location? = nil) {
        guard let tab = currentTab(), let (locations, action) = pasteSources() else { return NSSound.beep() }
        let target = destination ?? tab.location
        let sources = transferSources(locations, destination: target, action: action)
        guard !sources.isEmpty else { return NSSound.beep() }
        let id = operations.submit(action == .cut ? MutationKind.move : MutationKind.copy, sources: sources, destination: target)
        if action == .cut { services.clipboard.beginPaste(id) }
    }

    // MARK: transfer (drag and drop, copy to / move to)

    func transfer(_ sources: [Location], to destination: Location, copy: Bool) -> Bool {
        let accepted = transferSources(sources, destination: destination, action: copy ? ClipboardAction.copy : ClipboardAction.cut)
        guard !accepted.isEmpty else { return false }
        operations.submit(copy ? MutationKind.copy : MutationKind.move, sources: accepted, destination: destination)
        return true
    }

    func chooseFolderThenTransfer(_ items: [FileItem], copy: Bool) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.prompt = copy ? "Copy" : "Move"
        panel.message = copy ? "Choose where to copy the selected items." : "Choose where to move the selected items."
        let run: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            _ = self?.transfer(items.map(\.location), to: Location(localPath: url.path), copy: copy)
        }
        if let window = window() { panel.beginSheetModal(for: window, completionHandler: run) } else { run(panel.runModal()) }
    }

    // MARK: create, rename, delete

    func newFolder(in folder: Location? = nil) {
        guard let tab = currentTab() else { return }
        let parent = folder ?? tab.location
        guard CommandAvailability(selection: [], destinationCapabilities: tab.capabilities).canPaste || folder != nil else { return NSSound.beep() }
        var name = "untitled folder"
        var number = 1
        let exists: (String) -> Bool = { name in parent.localPath.map { FileManager.default.fileExists(atPath: ($0 as NSString).appendingPathComponent(name)) } ?? false }
        while exists(name) { number += 1; name = "untitled folder \(number)" }
        guard let child = try? namedChild(of: parent, name: name) else { return }
        operations.submit(.create, sources: [child])
    }

    func rename(_ item: FileItem, to name: String) {
        guard let target = try? namedChild(of: item.location.parent ?? item.location, name: name) else {
            return showAlert("Can’t rename", "“\(name)” isn’t a valid name.")
        }
        operations.submit(.rename, sources: [item.location], explicitTarget: target)
    }

    func trash(_ items: [FileItem]) {
        guard CommandAvailability(selection: items, destinationCapabilities: nil).canTrash else { return NSSound.beep() }
        operations.submit(.trash, sources: items.map(\.location))
    }

    func deletePermanently(_ items: [FileItem]) {
        guard CommandAvailability(selection: items, destinationCapabilities: nil).canDelete else { return NSSound.beep() }
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = items.count == 1 ? "Delete “\(items[0].name)” immediately?" : "Delete \(items.count) items immediately?"
        alert.informativeText = "This can’t be undone. The items will not go to the Trash."
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        let run: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            if response == .alertFirstButtonReturn { self?.operations.submit(.delete, sources: items.map(\.location), confirmedDelete: true) }
        }
        if let window = window() { alert.beginSheetModal(for: window, completionHandler: run) } else { run(alert.runModal()) }
    }

    // MARK: archives

    func extract(_ items: [FileItem]) {
        guard let tab = currentTab() else { return }
        for item in items where isExtractable(item) {
            guard let parent = item.location.parent, parent.localPath != nil || tab.location.localPath != nil else { continue }
            let destination = parent.localPath != nil ? parent : tab.location
            let started = ArchiveTools.extract(manager: operations, archive: item.location, parent: destination) { [weak self] message in
                self?.showAlert("Couldn’t extract “\(item.name)”", message)
            }
            if !started { showAlert("Couldn’t extract “\(item.name)”", "Extract works on archives on this Mac.") }
        }
    }

    func compress(_ items: [FileItem], format: ArchiveFormat) {
        guard CommandAvailability(selection: items, destinationCapabilities: nil).canCompress else { return NSSound.beep() }
        ArchiveTools.compress(manager: operations, sources: items.map(\.location), format: format) { [weak self] result in
            if case .failure(let error) = result, error != .cancelled { self?.showAlert("Couldn’t compress", error.message) }
        }
    }

    // MARK: open

    func open(_ items: [FileItem]) {
        for item in items { currentTab()?.open(item) }
    }

    func openWithMenu(for items: [FileItem], target: AnyObject) -> NSMenu {
        let menu = NSMenu()
        guard items.count == 1, !items[0].isDirectory, items[0].location.localPath != nil else { return menu }
        for app in OpenService.applications(toOpen: items[0].location) {
            let entry = NSMenuItem(title: app.name + (app.isDefault ? " (default)" : ""), action: #selector(openWithApplication(_:)), keyEquivalent: "")
            entry.target = self
            entry.representedObject = [items[0].location.uri, app.url.path]
            entry.image = NSWorkspace.shared.icon(forFile: app.url.path).resized(16)
            menu.addItem(entry)
        }
        if menu.items.isEmpty { menu.addItem(withTitle: "No Applications", action: nil, keyEquivalent: "") }
        return menu
    }

    @objc private func openWithApplication(_ sender: NSMenuItem) {
        guard let pair = sender.representedObject as? [String], pair.count == 2, let location = try? Location(uri: pair[0]) else { return }
        OpenService.open([location], with: URL(fileURLWithPath: pair[1]))
    }

    func openInTerminal(_ location: Location) {
        if !OpenService.openTerminal(at: location) { showAlert("Can’t open Terminal", "Terminal could not be opened here.") }
    }

    func copyText(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    // MARK: dispatch

    func perform(_ command: ContextCommand, items: [FileItem], shiftHeld: Bool = false) {
        guard let tab = currentTab() else { return }
        let target = items.first
        switch command {
        case .open: open(items)
        case .openWith: break
        case .extract: extract(items)
        case .compressZip: compress(items, format: .zip)
        case .compress7z: compress(items, format: .sevenZip)
        case .compressTarGz: compress(items, format: .tarGz)
        case .openInNewTab: items.filter(\.isDirectory).forEach { openInNewTab($0.location) }
        case .openInNewWindow: items.filter(\.isDirectory).forEach { openInNewWindow($0.location) }
        case .openInTerminal: openInTerminal(target?.location ?? tab.location)
        case .newFolder: newFolder()
        case .cut: copy(items, cut: true)
        case .copy: copy(items, cut: false)
        case .copyFilename: copyText(items.map(\.name).joined(separator: "\n"))
        case .copyFullPath: copyText(items.map { $0.location.localPath ?? $0.location.uri }.joined(separator: "\n"))
        case .copyTo: chooseFolderThenTransfer(items, copy: true)
        case .moveTo: chooseFolderThenTransfer(items, copy: false)
        case .paste: paste(into: target.flatMap { $0.isDirectory ? $0.location : nil })
        case .rename: if let target { tab.onRenameRequested?(target) }
        case .moveToTrash: trash(items)
        case .deletePermanently: deletePermanently(items)
        case .properties: items.forEach(showProperties)
        case .refresh: tab.refresh()
        case .toggleQuickAccess:
            let location = target?.location ?? tab.location
            if services.sidebarPreferences.isPinned(location) { services.sidebarPreferences.unpin(location) } else { services.sidebarPreferences.pin(location) }
            try? services.sidebarPreferences.save()
            NotificationCenter.default.post(name: .sidebarPinsChanged, object: nil)
        case .closeTab, .closeOtherTabs, .duplicateTab: break
        }
    }

    // MARK: alerts

    func showAlert(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        if let window = window() { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }
}

extension Notification.Name {
    static let sidebarPinsChanged = Notification.Name("casa.alera.MorinExplorer.sidebarPinsChanged")
}

extension NSImage {
    func resized(_ points: CGFloat) -> NSImage {
        let copy = (self.copy() as? NSImage) ?? self
        copy.size = NSSize(width: points, height: points)
        return copy
    }
}
