import AppKit
import ExplorerAdapters
import ExplorerDomain

private final class SectionItem {
    let section: SidebarSection
    init(_ section: SidebarSection) { self.section = section }
}

private final class MountItem {
    let volume: MountableVolume
    init(_ volume: MountableVolume) { self.volume = volume }
}

private final class SidebarOutline: NSOutlineView {
    var menuProvider: ((Int) -> NSMenu?)?
    override func menu(for event: NSEvent) -> NSMenu? { menuProvider?(row(at: convert(event.locationInWindow, from: nil))) }
}

/// Pinned folders, the user's folders, drives and attached devices, as a source list. Folders
/// expand in place; dropping files on one moves or copies them there.
final class SidebarViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    private let services: AppServices
    private let model: SidebarModel
    private let outline = SidebarOutline()
    private var sections: [SectionItem] = []
    private var parents: [ObjectIdentifier: AnyObject] = [:]
    private var selecting = false
    private var currentLocation: Location?
    private var observers: [NSObjectProtocol] = []

    var onNavigate: (Location) -> Void = { _ in }
    var onOpenNewTab: (Location) -> Void = { _ in }
    var onOpenNewWindow: (Location) -> Void = { _ in }
    var onDrop: ([Location], Location, Bool) -> Bool = { _, _, _ in false }

    init(services: AppServices) {
        self.services = services
        model = SidebarModel(preferences: services.sidebarPreferences, volumes: services.volumes)
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    deinit { observers.forEach(NotificationCenter.default.removeObserver) }

    override func loadView() {
        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("sidebar"))
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.dataSource = self
        outline.delegate = self
        outline.floatsGroupRows = false
        outline.rowSizeStyle = .default
        outline.registerForDraggedTypes([.fileURL])
        outline.setDraggingSourceOperationMask([.copy, .move], forLocal: false)
        outline.setAccessibilityLabel("Sidebar")
        outline.menuProvider = { [weak self] row in self?.menu(forRow: row) }
        view = scroll
        services.volumes.onChange = { [weak self] in self?.reload() }
        observers.append(NotificationCenter.default.addObserver(forName: .sidebarPinsChanged, object: nil, queue: .main) { [weak self] _ in self?.reload() })
        reload()
    }

    /// The labels of every visible row, top to bottom (used by tests and accessibility checks).
    var visibleLabels: [String] {
        (0..<outline.numberOfRows).compactMap { row in
            switch outline.item(atRow: row) {
            case let section as SectionItem: return section.section.title
            case let node as SidebarNode: return node.location.label
            case let mount as MountItem: return mount.volume.label
            default: return nil
            }
        }
    }

    // MARK: data

    func reload() {
        let selected = currentLocation
        sections = model.sections().map(SectionItem.init)
        parents.removeAll()
        selecting = true
        outline.reloadData()
        for section in sections where services.sidebarPreferences.isSectionExpanded(section.section.title) { outline.expandItem(section) }
        selecting = false
        if let selected { select(selected) }
    }

    /// Highlights the row for the tab's location when the sidebar has one.
    func select(_ location: Location) {
        currentLocation = location
        selecting = true
        defer { selecting = false }
        for row in 0..<outline.numberOfRows {
            if let node = outline.item(atRow: row) as? SidebarNode, node.location.location == location {
                outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                return
            }
        }
        outline.deselectAll(nil)
    }

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        switch item {
        case nil: return sections.count
        case let section as SectionItem: return section.section.nodes.count + section.section.mountables.count
        case let node as SidebarNode: return node.children.count
        default: return 0
        }
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        switch item {
        case nil: return sections[index]
        case let section as SectionItem:
            let nodes = section.section.nodes
            if index < nodes.count { parents[ObjectIdentifier(nodes[index])] = section; return nodes[index] }
            return MountItem(section.section.mountables[index - nodes.count])
        case let node as SidebarNode:
            let child = node.children[index]
            parents[ObjectIdentifier(child)] = node
            return child
        default: fatalError("unexpected sidebar item")
        }
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        switch item {
        case is SectionItem: return true
        case let node as SidebarNode: return node.isExpandable && node.location.location.localPath != nil || node.isExpandable && !node.children.isEmpty
        default: return false
        }
    }

    func outlineView(_ outlineView: NSOutlineView, isGroupItem item: Any) -> Bool { item is SectionItem }

    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool { !(item is SectionItem) }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        let cell = NSTableCellView()
        let label = NSTextField(labelWithString: "")
        label.lineBreakMode = .byTruncatingTail
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        cell.textField = label
        var constraints = [label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4), label.centerYAnchor.constraint(equalTo: cell.centerYAnchor)]
        switch item {
        case let section as SectionItem:
            label.stringValue = section.section.title
            constraints.append(label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2))
        case let node as SidebarNode:
            label.stringValue = node.location.label
            cell.imageView = iconView(Symbol.image(node.location.iconName, description: nil), in: cell, &constraints, label)
            cell.setAccessibilityLabel(node.location.label)
            if let path = node.location.location.localPath, services.volumes.mountedVolumes().first(where: { $0.path == path })?.isEjectable == true {
                let eject = NSButton(image: Symbol.image("eject.fill", description: "Eject") ?? NSImage(), target: self, action: #selector(ejectClicked(_:)))
                eject.isBordered = false
                eject.toolTip = "Eject"
                eject.identifier = NSUserInterfaceItemIdentifier(path)
                eject.translatesAutoresizingMaskIntoConstraints = false
                cell.addSubview(eject)
                constraints = constraints.filter { $0.firstItem as? NSTextField !== label || $0.firstAttribute != .trailing }
                constraints += [eject.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4), eject.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                                label.trailingAnchor.constraint(equalTo: eject.leadingAnchor, constant: -4)]
            }
        case let mount as MountItem:
            label.stringValue = mount.volume.label
            label.textColor = .secondaryLabelColor
            cell.imageView = iconView(Symbol.image(mount.volume.iconName, description: nil), in: cell, &constraints, label)
            cell.toolTip = "Click to mount"
        default: break
        }
        NSLayoutConstraint.activate(constraints)
        return cell
    }

    private func iconView(_ image: NSImage?, in cell: NSTableCellView, _ constraints: inout [NSLayoutConstraint], _ label: NSTextField) -> NSImageView {
        let icon = NSImageView(image: image ?? NSImage())
        icon.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(icon)
        constraints += [icon.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2), icon.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                        icon.widthAnchor.constraint(equalToConstant: 18), label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6)]
        return icon
    }

    // MARK: expansion

    private func occurrence(of item: Any) -> [String]? {
        var path: [String] = []
        var cursor: AnyObject? = item as AnyObject
        while let current = cursor {
            if let section = current as? SectionItem { path.insert(section.section.title, at: 0); return path }
            if let node = current as? SidebarNode { path.insert(node.location.location.uri, at: 0); cursor = parents[ObjectIdentifier(node)] } else { return nil }
        }
        return nil
    }

    func outlineViewItemWillExpand(_ notification: Notification) {
        guard let item = notification.userInfo?["NSObject"] else { return }
        if let node = item as? SidebarNode, node.children.isEmpty { loadChildren(of: node) }
    }

    func outlineViewItemDidExpand(_ notification: Notification) { record(notification, expanded: true) }
    func outlineViewItemDidCollapse(_ notification: Notification) { record(notification, expanded: false) }

    private func record(_ notification: Notification, expanded: Bool) {
        guard !selecting, let item = notification.userInfo?["NSObject"] else { return }
        if let section = item as? SectionItem { services.sidebarPreferences.setSectionExpanded(section.section.title, expanded) }
        else if let occurrence = occurrence(of: item) { services.sidebarPreferences.setOccurrenceExpanded(occurrence, expanded) }
        try? services.sidebarPreferences.save()
    }

    private func loadChildren(of node: SidebarNode) {
        services.directories.list(DirectoryRequest(location: node.location.location)) { [weak self, weak node] completion in
            guard let self, let node, case .succeeded(let snapshot) = completion.outcome else { return }
            node.children = SidebarModel.children(from: snapshot)
            self.selecting = true
            self.outline.reloadItem(node, reloadChildren: true)
            self.selecting = false
            for child in node.children {
                if let occurrence = self.occurrence(of: child), self.services.sidebarPreferences.isOccurrenceExpanded(occurrence) { self.outline.expandItem(child) }
            }
            if let current = self.currentLocation { self.select(current) }
        }
    }

    // MARK: selection

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !selecting else { return }
        switch outline.item(atRow: outline.selectedRow) {
        case let node as SidebarNode: onNavigate(node.location.location)
        case let mount as MountItem:
            services.volumes.mount(id: mount.volume.id) { [weak self] ok in
                if !ok { self?.alert("Couldn’t mount “\(mount.volume.label)”", "The volume could not be mounted.") }
                self?.reload()
            }
        default: break
        }
    }

    @objc private func ejectClicked(_ sender: NSButton) {
        guard let path = sender.identifier?.rawValue else { return }
        SystemVolumes.eject(path: path) { [weak self] error in
            if let error { self?.alert("Couldn’t eject", error.localizedDescription) }
            self?.reload()
        }
    }

    private func alert(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        if let window = view.window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    // MARK: context menu

    private func menu(forRow row: Int) -> NSMenu? {
        guard let node = outline.item(atRow: row) as? SidebarNode else { return nil }
        let location = node.location.location
        let menu = NSMenu()
        menu.autoenablesItems = false
        let isDirectory = true
        func add(_ title: String, _ action: Selector) { let item = NSMenuItem(title: title, action: action, keyEquivalent: ""); item.target = self; item.representedObject = location.uri; menu.addItem(item) }
        add("Open", #selector(menuOpen(_:)))
        if isDirectory { add("Open in New Tab", #selector(menuNewTab(_:))); add("Open in New Window", #selector(menuNewWindow(_:))) }
        menu.addItem(.separator())
        add(services.sidebarPreferences.isPinned(location) ? "Remove from Quick Access" : "Add to Quick Access", #selector(menuTogglePin(_:)))
        if let path = location.localPath, services.volumes.mountedVolumes().first(where: { $0.path == path })?.isEjectable == true { add("Eject", #selector(menuEject(_:))) }
        return menu
    }

    private func location(from sender: NSMenuItem) -> Location? { (sender.representedObject as? String).flatMap { try? Location(uri: $0) } }
    @objc private func menuOpen(_ sender: NSMenuItem) { location(from: sender).map(onNavigate) }
    @objc private func menuNewTab(_ sender: NSMenuItem) { location(from: sender).map(onOpenNewTab) }
    @objc private func menuNewWindow(_ sender: NSMenuItem) { location(from: sender).map(onOpenNewWindow) }
    @objc private func menuEject(_ sender: NSMenuItem) { if let path = location(from: sender)?.localPath { SystemVolumes.eject(path: path) { [weak self] _ in self?.reload() } } }
    @objc private func menuTogglePin(_ sender: NSMenuItem) {
        guard let location = location(from: sender) else { return }
        if services.sidebarPreferences.isPinned(location) { services.sidebarPreferences.unpin(location) } else { services.sidebarPreferences.pin(location) }
        try? services.sidebarPreferences.save()
        NotificationCenter.default.post(name: .sidebarPinsChanged, object: nil)
    }

    // MARK: drag and drop

    func outlineView(_ outlineView: NSOutlineView, validateDrop info: NSDraggingInfo, proposedItem item: Any?, proposedChildIndex index: Int) -> NSDragOperation {
        let urls = DropSupport.urls(from: info)
        guard !urls.isEmpty else { return [] }
        if let node = item as? SidebarNode, node.location.location.localPath != nil {
            outlineView.setDropItem(node, dropChildIndex: NSOutlineViewDropOnItemIndex)
            let sources = urls.map { Location(localPath: $0.path) }
            guard !transferSources(sources, destination: node.location.location, action: .copy).isEmpty else { return [] }
            return DropSupport.operation(for: urls, destination: node.location.location, mask: info.draggingSourceOperationMask, flags: NSEvent.modifierFlags)
        }
        if let section = item as? SectionItem, section.section.title == SidebarPreferences.quickAccess,
           urls.allSatisfy({ (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }) {
            outlineView.setDropItem(section, dropChildIndex: NSOutlineViewDropOnItemIndex)
            return .link
        }
        return []
    }

    func outlineView(_ outlineView: NSOutlineView, acceptDrop info: NSDraggingInfo, item: Any?, childIndex index: Int) -> Bool {
        let urls = DropSupport.urls(from: info)
        if let section = item as? SectionItem, section.section.title == SidebarPreferences.quickAccess {
            urls.forEach { services.sidebarPreferences.pin(Location(localPath: $0.path)) }
            try? services.sidebarPreferences.save()
            reload()
            return true
        }
        guard let node = item as? SidebarNode else { return false }
        let operation = DropSupport.operation(for: urls, destination: node.location.location, mask: info.draggingSourceOperationMask, flags: NSEvent.modifierFlags)
        return onDrop(urls.map { Location(localPath: $0.path) }, node.location.location, operation == .copy)
    }
}
