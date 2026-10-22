import AppKit
import ExplorerAdapters
import ExplorerDomain

/// Every window is one tab: macOS groups them with its own tab bar. The manager creates them,
/// keeps them alive and knows which locations are open for session restore.
final class WindowManager {
    private let services: AppServices
    private(set) var controllers: [ExplorerWindowController] = []
    private let tabbingIdentifier = "casa.alera.MorinExplorer.main"

    init(services: AppServices) { self.services = services }

    var locations: [Location] { orderedControllers.map { $0.tab.location } }

    /// Windows in tab-bar order, grouped by window.
    private var orderedControllers: [ExplorerWindowController] {
        var seen = Set<ObjectIdentifier>()
        var result: [ExplorerWindowController] = []
        for controller in controllers {
            for tabbed in controller.window?.tabbedWindows ?? [controller.window].compactMap({ $0 }) {
                if let match = controllers.first(where: { $0.window === tabbed }), seen.insert(ObjectIdentifier(match)).inserted { result.append(match) }
            }
        }
        return result
    }

    @discardableResult
    func open(_ location: Location, asTabIn existing: NSWindow? = nil, activate: Bool = true) -> ExplorerWindowController {
        let controller = ExplorerWindowController(location: location, services: services, manager: self)
        controllers.append(controller)
        guard let window = controller.window else { return controller }
        window.tabbingIdentifier = tabbingIdentifier
        if let existing {
            existing.tabbingMode = .preferred
            window.tabbingMode = .preferred
            existing.addTabbedWindow(window, ordered: .above)
            if activate { window.makeKeyAndOrderFront(nil) } else { window.orderFront(nil) }
        } else {
            // A window of its own: it must not join the key window's tab bar when it first appears.
            window.tabbingMode = .disallowed
            window.makeKeyAndOrderFront(nil)
            window.tabbingMode = .preferred
        }
        controller.start()
        return controller
    }

    func closed(_ controller: ExplorerWindowController) {
        controllers.removeAll { $0 === controller }
        controller.tab.close()
    }

    var hasWindows: Bool { !controllers.isEmpty }
}

private extension NSToolbarItem.Identifier {
    static let back = NSToolbarItem.Identifier("back")
    static let forward = NSToolbarItem.Identifier("forward")
    static let navigation = NSToolbarItem.Identifier("navigation")
    static let up = NSToolbarItem.Identifier("up")
    static let viewMode = NSToolbarItem.Identifier("viewMode")
    static let options = NSToolbarItem.Identifier("options")
    static let search = NSToolbarItem.Identifier("search")
    static let newFolder = NSToolbarItem.Identifier("newFolder")
}

final class ExplorerWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate, NSSearchFieldDelegate, NSMenuItemValidation {
    let tab: TabModel
    let services: AppServices
    private unowned let manager: WindowManager
    private(set) var commands: FileCommands!
    private(set) var browser: BrowserViewController!
    private(set) var sidebar: SidebarViewController!
    private var split: NSSplitViewController!
    private var searchItem: NSSearchToolbarItem?
    private var viewModeGroup: NSToolbarItemGroup?
    private var followUp: OperationsFollowUp?
    private var properties: [PropertiesWindowController] = []
    private var searchMode = SearchMode.filename
    private let initial: Location

    init(location: Location, services: AppServices, manager: WindowManager) {
        self.services = services
        self.manager = manager
        initial = location
        tab = TabModel(location: location, services: services)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 700), styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.setFrameAutosaveName("MorinExplorerMainWindow")
        window.minSize = NSSize(width: 640, height: 400)
        super.init(window: window)
        window.delegate = self
        commands = FileCommands(services: services, tab: { [weak self] in self?.tab }, window: { [weak self] in self?.window })
        commands.openInNewTab = { [weak self] location in self.map { $0.manager.open(location, asTabIn: $0.window) } }
        commands.openInNewWindow = { [weak self] location in self?.manager.open(location) }
        commands.showProperties = { [weak self] item in self?.showProperties(item) }
        browser = BrowserViewController(tab: tab, services: services, commands: commands)
        browser.onTitleChanged = { [weak self] in self?.titleChanged() }
        browser.onShowOperations = { [weak self] anchor in self?.showOperations(anchor) }
        sidebar = SidebarViewController(services: services)
        sidebar.onNavigate = { [weak self] location in self?.tab.navigate(to: location) }
        sidebar.onOpenNewTab = { [weak self] location in self.map { $0.manager.open(location, asTabIn: $0.window) } }
        sidebar.onOpenNewWindow = { [weak self] location in self?.manager.open(location) }
        sidebar.onDrop = { [weak self] sources, destination, copy in self?.commands.transfer(sources, to: destination, copy: copy) ?? false }
        split = NSSplitViewController()
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = 170
        sidebarItem.maximumThickness = 320
        sidebarItem.canCollapse = true
        let browserItem = NSSplitViewItem(viewController: browser)
        split.addSplitViewItem(sidebarItem)
        split.addSplitViewItem(browserItem)
        split.splitView.autosaveName = "MorinExplorerSplit"
        window.contentViewController = split
        window.setContentSize(NSSize(width: 1100, height: 700))
        let toolbar = NSToolbar(identifier: "MorinExplorerToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar
        window.toolbarStyle = .unified
        followUp = OperationsFollowUp(services: services) { [weak self] in self?.window }
        window.setAccessibilityIdentifier("explorer-window")
        titleChanged()
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Begins browsing; called once the window is on screen.
    func start() {
        tab.navigate(to: initial)
        sidebar.select(initial)
    }

    // MARK: window

    func windowWillClose(_ notification: Notification) { manager.closed(self) }

    func windowDidBecomeKey(_ notification: Notification) {
        if !(window?.firstResponder is NSTextView) { browser.focusFiles() }
    }

    private func titleChanged() {
        window?.title = tab.title
        window?.tab.title = tab.title
        window?.tab.toolTip = tab.location.localPath ?? tab.location.uri
        window?.representedURL = tab.location.localPath.map { URL(fileURLWithPath: $0) }
        sidebar.select(tab.location)
        if !tab.isSearching, let field = searchItem?.searchField, !field.stringValue.isEmpty, field.currentEditor() == nil { field.stringValue = "" }
        validateToolbar()
    }

    private func validateToolbar() { window?.toolbar?.validateVisibleItems() }

    private func showProperties(_ item: FileItem) {
        let controller = PropertiesWindowController(item: item, services: services)
        controller.onClose = { [weak self, weak controller] in self?.properties.removeAll { $0 === controller } }
        properties.append(controller)
        controller.showWindow(nil)
    }

    private func showOperations(_ anchor: NSView) {
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentViewController = OperationsViewController(services: services)
        popover.show(relativeTo: anchor.bounds, of: anchor, preferredEdge: .maxY)
    }

    // MARK: toolbar

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .navigation, .up, .newFolder, .flexibleSpace, .viewMode, .options, .search]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] { toolbarDefaultItemIdentifiers(toolbar) + [.space] }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier, willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        switch identifier {
        case .navigation:
            let group = NSToolbarItemGroup(itemIdentifier: identifier, images: [Symbol.image("chevron.left", description: "Back")!, Symbol.image("chevron.right", description: "Forward")!],
                                           selectionMode: .momentary, labels: ["Back", "Forward"], target: self, action: #selector(navigationSegment(_:)))
            group.label = "Navigation"
            group.subitems.first?.toolTip = "Back"
            group.subitems.last?.toolTip = "Forward"
            group.subitems.first?.autovalidates = false
            return group
        case .up:
            return button(identifier, "Up", "arrow.up", #selector(goUp(_:)), "Enclosing folder")
        case .newFolder:
            return button(identifier, "New Folder", "folder.badge.plus", #selector(newFolder(_:)), "New folder")
        case .viewMode:
            let images = ViewMode.allCases.map { Symbol.image(Self.symbol(for: $0), description: Self.name(for: $0))! }
            let group = NSToolbarItemGroup(itemIdentifier: identifier, images: images, selectionMode: .selectOne,
                                           labels: ViewMode.allCases.map(Self.name(for:)), target: self, action: #selector(viewModeSegment(_:)))
            group.label = "View"
            group.selectedIndex = ViewMode.allCases.firstIndex(of: tab.options.viewMode) ?? 0
            for (index, mode) in ViewMode.allCases.enumerated() { group.subitems[index].toolTip = Self.name(for: mode) }
            viewModeGroup = group
            return group
        case .options:
            let item = NSMenuToolbarItem(itemIdentifier: identifier)
            item.image = Symbol.image("slider.horizontal.3", description: "View options")
            item.label = "Options"
            item.toolTip = "Sort, group and view options"
            item.menu = viewMenu()
            item.showsIndicator = true
            return item
        case .search:
            let item = NSSearchToolbarItem(itemIdentifier: identifier)
            item.searchField.delegate = self
            item.searchField.placeholderString = "Search"
            item.searchField.sendsSearchStringImmediately = true
            item.searchField.sendsWholeSearchString = false
            item.searchField.setAccessibilityLabel("Search")
            item.searchField.setAccessibilityIdentifier("search-field")
            (item.searchField.cell as? NSSearchFieldCell)?.searchMenuTemplate = searchModeMenu()
            item.resignsFirstResponderWithCancel = true
            searchItem = item
            return item
        default:
            return nil
        }
    }

    private func button(_ identifier: NSToolbarItem.Identifier, _ label: String, _ symbol: String, _ action: Selector, _ tip: String) -> NSToolbarItem {
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = label
        item.toolTip = tip
        item.image = Symbol.image(symbol, description: label)
        item.target = self
        item.action = action
        item.isBordered = true
        return item
    }

    static func symbol(for mode: ViewMode) -> String {
        switch mode {
        case .detailedList: return "list.bullet"
        case .compactList: return "list.dash"
        case .iconGrid: return "square.grid.2x2"
        case .hybrid: return "rectangle.grid.1x2"
        }
    }

    static func name(for mode: ViewMode) -> String {
        switch mode {
        case .detailedList: return "Detailed List"
        case .compactList: return "Compact List"
        case .iconGrid: return "Icon Grid"
        case .hybrid: return "Hybrid"
        }
    }

    @objc private func navigationSegment(_ sender: NSToolbarItemGroup) {
        if sender.selectedIndex == 0 { goBack(nil) } else { goForward(nil) }
    }

    @objc private func viewModeSegment(_ sender: NSToolbarItemGroup) {
        guard ViewMode.allCases.indices.contains(sender.selectedIndex) else { return }
        tab.updateOptions { $0.viewMode = ViewMode.allCases[sender.selectedIndex] }
    }

    // MARK: search

    private func searchModeMenu() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = true
        let names = menu.addItem(withTitle: "File Names", action: #selector(chooseSearchMode(_:)), keyEquivalent: "")
        names.tag = 0
        names.target = self
        let contents = menu.addItem(withTitle: "File Contents", action: #selector(chooseSearchMode(_:)), keyEquivalent: "")
        contents.tag = 1
        contents.target = self
        return menu
    }

    @objc private func chooseSearchMode(_ sender: NSMenuItem) {
        searchMode = sender.tag == 1 ? .content : .filename
        searchItem?.searchField.placeholderString = searchMode == .content ? "Search contents" : "Search"
        runSearch()
    }

    func controlTextDidChange(_ obj: Notification) {
        guard (obj.object as? NSSearchField) === searchItem?.searchField else { return }
        runSearch()
    }

    private func runSearch() {
        let text = searchItem?.searchField.stringValue ?? ""
        if text.trimmingCharacters(in: .whitespaces).isEmpty { tab.endSearch(); return }
        tab.beginSearch(text, mode: searchMode)
    }

    func searchFieldDidEndSearching(_ sender: NSSearchField) {
        tab.endSearch()
        browser.focusFiles()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard control === searchItem?.searchField else { return false }
        if selector == #selector(NSResponder.insertNewline(_:)) {
            tab.recordSearchTerm(control.stringValue)
            runSearch()
            browser.focusFiles()
            return true
        }
        return false
    }

    // MARK: navigation actions

    @objc func goBack(_ sender: Any?) { tab.back() }
    @objc func goForward(_ sender: Any?) { tab.forward() }
    @objc func goUp(_ sender: Any?) { tab.up() }
    @objc func refresh(_ sender: Any?) { tab.refresh() }
    @objc func goHome(_ sender: Any?) { tab.navigate(to: Location.home) }
    @objc func focusLocation(_ sender: Any?) { browser.locationBar.beginEditing() }
    @objc func focusSearch(_ sender: Any?) { window?.makeFirstResponder(searchItem?.searchField) }
    @objc func showOperationsPage(_ sender: Any?) { showOperations(browser.statusBar) }

    @objc func newTab(_ sender: Any?) { manager.open(newTabLocation(), asTabIn: window) }
    @objc override func newWindowForTab(_ sender: Any?) { newTab(sender) }
    @objc func newWindow(_ sender: Any?) { manager.open(newTabLocation()) }
    @objc func duplicateTab(_ sender: Any?) { manager.open(tab.location, asTabIn: window) }

    @objc func closeOtherTabs(_ sender: Any?) {
        for other in (window?.tabbedWindows ?? []) where other !== window { other.close() }
    }

    private func newTabLocation() -> Location {
        (try? Location(uri: services.preferences.preferences.newTabLocation)) ?? Location.home
    }

    // MARK: file actions (the responder chain finds these from menus, keys and toolbar)

    private var selection: [FileItem] { tab.selectedItems }

    @objc func openSelection(_ sender: Any?) { commands.perform(.open, items: selection) }
    @objc func openInNewTabAction(_ sender: Any?) { commands.perform(.openInNewTab, items: selection) }
    @objc func openInNewWindowAction(_ sender: Any?) { commands.perform(.openInNewWindow, items: selection) }
    @objc func openInTerminal(_ sender: Any?) { commands.perform(.openInTerminal, items: selection) }
    @objc func copy(_ sender: Any?) { commands.perform(.copy, items: selection) }
    @objc func cut(_ sender: Any?) { commands.perform(.cut, items: selection) }
    @objc func paste(_ sender: Any?) { commands.perform(.paste, items: []) }
    @objc func pasteIntoFolder(_ sender: Any?) { commands.perform(.paste, items: selection.count == 1 ? selection : []) }
    @objc func copyName(_ sender: Any?) { commands.perform(.copyFilename, items: selection) }
    @objc func copyPath(_ sender: Any?) { commands.perform(.copyFullPath, items: selection.isEmpty ? [] : selection) }
    @objc func copyTo(_ sender: Any?) { commands.perform(.copyTo, items: selection) }
    @objc func moveTo(_ sender: Any?) { commands.perform(.moveTo, items: selection) }
    @objc func newFolder(_ sender: Any?) { commands.newFolder() }
    @objc func renameItem(_ sender: Any?) { commands.perform(.rename, items: selection) }
    @objc func moveToTrash(_ sender: Any?) { commands.perform(.moveToTrash, items: selection) }
    @objc override func deleteBackward(_ sender: Any?) { moveToTrash(sender) }
    @objc func deletePermanently(_ sender: Any?) { commands.perform(.deletePermanently, items: selection) }
    @objc func getInfo(_ sender: Any?) { commands.perform(.properties, items: selection.isEmpty ? [] : selection) }
    @objc func extractHere(_ sender: Any?) { commands.perform(.extract, items: selection) }
    @objc func compressZip(_ sender: Any?) { commands.perform(.compressZip, items: selection) }
    @objc func compress7z(_ sender: Any?) { commands.perform(.compress7z, items: selection) }
    @objc func compressTarGz(_ sender: Any?) { commands.perform(.compressTarGz, items: selection) }
    @objc func togglePin(_ sender: Any?) { commands.perform(.toggleQuickAccess, items: selection.count == 1 ? selection : []) }

    // MARK: view actions

    @objc func setViewMode(_ sender: NSMenuItem) {
        guard ViewMode.allCases.indices.contains(sender.tag) else { return }
        tab.updateOptions { $0.viewMode = ViewMode.allCases[sender.tag] }
    }

    @objc func setGridSize(_ sender: NSMenuItem) {
        guard GridSize.allCases.indices.contains(sender.tag) else { return }
        tab.updateOptions { $0.gridSize = GridSize.allCases[sender.tag] }
    }

    @objc func zoomIn(_ sender: Any?) { stepGridSize(1) }
    @objc func zoomOut(_ sender: Any?) { stepGridSize(-1) }

    private func stepGridSize(_ delta: Int) {
        let all = GridSize.allCases
        guard let index = all.firstIndex(of: tab.options.gridSize), all.indices.contains(index + delta) else { return NSSound.beep() }
        tab.updateOptions { $0.gridSize = all[index + delta] }
    }

    @objc func setSortField(_ sender: NSMenuItem) {
        guard SortField.allCases.indices.contains(sender.tag) else { return }
        tab.updateOptions { $0.sortField = SortField.allCases[sender.tag] }
    }

    @objc func setSortDirection(_ sender: NSMenuItem) { tab.updateOptions { $0.sortDirection = sender.tag == 0 ? .ascending : .descending } }

    @objc func setGroupField(_ sender: NSMenuItem) {
        guard GroupField.allCases.indices.contains(sender.tag) else { return }
        tab.updateOptions { $0.groupField = GroupField.allCases[sender.tag] }
    }

    @objc func toggleFoldersFirst(_ sender: Any?) { tab.updateOptions { $0.foldersFirst.toggle() } }
    @objc func toggleHidden(_ sender: Any?) { tab.updateOptions { $0.showHidden.toggle() } }
    @objc func toggleThumbnails(_ sender: Any?) { tab.updateOptions { $0.thumbnails.toggle() } }

    @objc func resetFolderView(_ sender: Any?) {
        guard let path = tab.location.localPath else { return }
        services.viewPreferences.clear(path)
        try? services.viewPreferences.save()
        tab.setOptions(services.viewPreferences.options(for: path, items: tab.displayed), forFolder: false)
    }

    /// The menu used by the toolbar's options button and the View menu's sort/group sections.
    func viewMenu() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = true
        func submenu(_ title: String, _ fill: (NSMenu) -> Void) {
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let sub = NSMenu(title: title)
            fill(sub)
            item.submenu = sub
            menu.addItem(item)
        }
        submenu("View") { sub in for (index, mode) in ViewMode.allCases.enumerated() { sub.add(Self.name(for: mode), #selector(setViewMode(_:)), target: self, tag: index) } }
        submenu("Icon Size") { sub in for (index, size) in GridSize.allCases.enumerated() { sub.add(size.rawValue.replacingOccurrences(of: "-", with: " ").capitalized, #selector(setGridSize(_:)), target: self, tag: index) } }
        submenu("Sort By") { sub in
            for (index, field) in SortField.allCases.enumerated() { sub.add(Self.sortName(field), #selector(setSortField(_:)), target: self, tag: index) }
            sub.addItem(.separator())
            sub.add("Ascending", #selector(setSortDirection(_:)), target: self, tag: 0)
            sub.add("Descending", #selector(setSortDirection(_:)), target: self, tag: 1)
            sub.addItem(.separator())
            sub.add("Folders First", #selector(toggleFoldersFirst(_:)), target: self)
        }
        submenu("Group By") { sub in for (index, field) in GroupField.allCases.enumerated() { sub.add(Self.groupName(field), #selector(setGroupField(_:)), target: self, tag: index) } }
        menu.addItem(.separator())
        menu.add("Show Hidden Files", #selector(toggleHidden(_:)), key: ".", modifiers: [.command, .shift], target: self)
        menu.add("Show Thumbnails", #selector(toggleThumbnails(_:)), target: self)
        menu.addItem(.separator())
        menu.add("Reset This Folder’s View", #selector(resetFolderView(_:)), target: self)
        return menu
    }

    static func sortName(_ field: SortField) -> String {
        switch field { case .name: return "Name"; case .dateModified: return "Date Modified"; case .type: return "Kind"; case .size: return "Size" }
    }

    static func groupName(_ field: GroupField) -> String {
        switch field { case .none: return "None"; case .type: return "Kind"; case .size: return "Size"; case .dateModified: return "Date Modified" }
    }

    // MARK: validation

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        guard let action = item.action else { return false }
        let selected = selection
        let availability = CommandAvailability(selection: selected, destinationCapabilities: tab.isSearching ? nil : tab.capabilities)
        let options = tab.options
        func check(_ on: Bool) { item.state = on ? .on : .off }
        switch action {
        case #selector(goBack(_:)): return tab.canGoBack
        case #selector(goForward(_:)): return tab.canGoForward
        case #selector(goUp(_:)): return tab.parent != nil
        case #selector(openSelection(_:)): return availability.canOpen
        case #selector(openInNewTabAction(_:)), #selector(openInNewWindowAction(_:)): return availability.canOpen && selected.contains(where: \.isDirectory)
        case #selector(openInTerminal(_:)): return selected.isEmpty ? tab.location.localPath != nil : availability.canOpenTerminal
        case #selector(copy(_:)), #selector(copyName(_:)), #selector(copyTo(_:)): return availability.canCopy
        case #selector(copyPath(_:)): return !selected.isEmpty && availability.canCopy
        case #selector(cut(_:)), #selector(moveTo(_:)): return availability.canCut
        case #selector(paste(_:)): return commands.canPaste
        case #selector(pasteIntoFolder(_:)): return selected.count == 1 && selected[0].isDirectory && availability.canPaste && commands.canPaste
        case #selector(newFolder(_:)): return !tab.isSearching && CommandAvailability(selection: [], destinationCapabilities: tab.capabilities).canPaste
        case #selector(renameItem(_:)): return availability.canRename
        case #selector(moveToTrash(_:)): return availability.canTrash
        case #selector(deletePermanently(_:)): return availability.canDelete
        case #selector(getInfo(_:)): return availability.canOpen
        case #selector(extractHere(_:)): return availability.canExtract
        case #selector(compressZip(_:)), #selector(compress7z(_:)), #selector(compressTarGz(_:)): return availability.canCompress
        case #selector(togglePin(_:)):
            let location = selected.count == 1 ? selected[0].location : tab.location
            item.title = services.sidebarPreferences.isPinned(location) ? "Remove from Quick Access" : "Add to Quick Access"
            return location.localPath != nil
        case #selector(setViewMode(_:)): check(ViewMode.allCases[safe: item.tag] == options.viewMode); return true
        case #selector(setGridSize(_:)): check(GridSize.allCases[safe: item.tag] == options.gridSize); return true
        case #selector(setSortField(_:)): check(SortField.allCases[safe: item.tag] == options.sortField); return true
        case #selector(setSortDirection(_:)): check((item.tag == 0) == (options.sortDirection == .ascending)); return true
        case #selector(setGroupField(_:)): check(GroupField.allCases[safe: item.tag] == options.groupField); return true
        case #selector(toggleFoldersFirst(_:)): check(options.foldersFirst); return true
        case #selector(toggleHidden(_:)): check(options.showHidden); return true
        case #selector(toggleThumbnails(_:)): check(options.thumbnails); return true
        case #selector(zoomIn(_:)), #selector(zoomOut(_:)): return options.viewMode == .iconGrid || options.viewMode == .hybrid
        case #selector(resetFolderView(_:)): return tab.location.localPath != nil
        case #selector(chooseSearchMode(_:)):
            let content = item.tag == 1
            check(content == (searchMode == .content))
            item.toolTip = content && !services.preferences.preferences.contentSearchEnabled ? "Turn on content search in Settings" : nil
            return !content || services.preferences.preferences.contentSearchEnabled
        default: return true
        }
    }
}

extension ExplorerWindowController: NSToolbarItemValidation {
    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        switch item.itemIdentifier {
        case .up: return tab.parent != nil
        case .newFolder: return !tab.isSearching && CommandAvailability(selection: [], destinationCapabilities: tab.capabilities).canPaste
        case .navigation:
            if let group = item as? NSToolbarItemGroup, group.subitems.count == 2 {
                group.subitems[0].isEnabled = tab.canGoBack
                group.subitems[1].isEnabled = tab.canGoForward
            }
            return true
        case .viewMode:
            (item as? NSToolbarItemGroup).map { $0.selectedIndex = ViewMode.allCases.firstIndex(of: tab.options.viewMode) ?? 0 }
            return true
        default: return true
        }
    }
}
