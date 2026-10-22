import AppKit
import ExplorerAdapters
import ExplorerDomain

/// Get Info: the details of one item on a General tab, and its permission bits on a Permissions tab.
final class PropertiesWindowController: NSWindowController, NSWindowDelegate {
    var onClose: () -> Void = {}
    private let item: FileItem
    private let services: AppServices
    private var details: PropertyDetails?
    private var rows: [(String, NSTextField)] = []
    private var checkboxes: [[NSButton]] = []
    private let status = NSTextField(labelWithString: "")
    private let sizeField = NSTextField(labelWithString: "")
    private let apply = NSButton(title: "Apply", target: nil, action: nil)
    private var sizeToken: CancellationToken?

    init(item: FileItem, services: AppServices) {
        self.item = item
        self.services = services
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 420, height: 420), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "\(item.name) Info"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        window.setAccessibilityIdentifier("properties-window")
        build()
        load()
        window.center()
    }

    required init?(coder: NSCoder) { fatalError() }

    func windowWillClose(_ notification: Notification) {
        sizeToken?.cancel()
        onClose()
    }

    // MARK: layout

    private func build() {
        let tabs = NSTabView()
        let general = NSTabViewItem(identifier: "general")
        general.label = "General"
        general.view = generalView()
        let permissions = NSTabViewItem(identifier: "permissions")
        permissions.label = "Permissions"
        permissions.view = permissionsView()
        tabs.addTabViewItem(general)
        tabs.addTabViewItem(permissions)
        tabs.translatesAutoresizingMaskIntoConstraints = false
        status.textColor = .secondaryLabelColor
        status.translatesAutoresizingMaskIntoConstraints = false
        guard let content = window?.contentView else { return }
        content.addSubview(tabs)
        content.addSubview(status)
        NSLayoutConstraint.activate([
            tabs.topAnchor.constraint(equalTo: content.topAnchor, constant: 12), tabs.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 12),
            tabs.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -12), tabs.bottomAnchor.constraint(equalTo: status.topAnchor, constant: -8),
            status.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16), status.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            status.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -12),
        ])
    }

    private func generalView() -> NSView {
        let icon = NSImageView(image: IconCache.shared.icon(for: item, points: 64))
        icon.setAccessibilityElement(false)
        let name = NSTextField(labelWithString: item.name)
        name.font = .boldSystemFont(ofSize: 15)
        name.lineBreakMode = .byTruncatingMiddle
        let header = NSStackView(views: [icon, name])
        header.spacing = 12
        let grid = NSGridView()
        for label in ["Kind", "Size", "Where", "Created", "Modified", "Last opened", "Owner", "Group"] {
            let value = NSTextField(wrappingLabelWithString: "…")
            value.isSelectable = true
            value.setAccessibilityLabel(label)
            let title = NSTextField(labelWithString: label + ":")
            title.alignment = .right
            title.textColor = .secondaryLabelColor
            grid.addRow(with: [title, value])
            rows.append((label, value))
            if label == "Size" { sizeField.stringValue = ""; rows[rows.count - 1].1 = value }
        }
        grid.column(at: 0).xPlacement = .trailing
        grid.rowSpacing = 8
        grid.columnSpacing = 10
        let stack = NSStackView(views: [header, grid])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 18
        stack.edgeInsets = NSEdgeInsets(top: 14, left: 14, bottom: 14, right: 14)
        return stack
    }

    private func permissionsView() -> NSView {
        let grid = NSGridView()
        let header = ["", "Read", "Write", "Execute"].map { NSTextField(labelWithString: $0) as NSView }
        grid.addRow(with: header)
        for who in ["Owner", "Group", "Others"] {
            let boxes = (0..<3).map { _ in NSButton(checkboxWithTitle: "", target: self, action: #selector(permissionChanged)) }
            let names = ["Read", "Write", "Execute"]
            for (index, box) in boxes.enumerated() { box.setAccessibilityLabel("\(who) \(names[index])") }
            checkboxes.append(boxes)
            grid.addRow(with: [NSTextField(labelWithString: who + ":")] + boxes)
        }
        grid.rowSpacing = 10
        grid.columnSpacing = 24
        apply.target = self
        apply.action = #selector(applyPermissions)
        apply.bezelStyle = .rounded
        apply.isEnabled = false
        let stack = NSStackView(views: [grid, apply])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 18
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 18, bottom: 14, right: 14)
        return stack
    }

    // MARK: data

    private func load() {
        guard let path = item.location.localPath else {
            status.stringValue = "Some details aren’t available for items inside an archive or on a remote location."
            set("Kind", Formatters.type(item))
            set("Size", Formatters.size(item))
            set("Where", item.location.parent?.displayName ?? "—")
            set("Modified", Formatters.date(item.modified))
            for label in ["Created", "Last opened", "Owner", "Group"] { set(label, "Not available") }
            checkboxes.flatMap { $0 }.forEach { $0.isEnabled = false }
            return
        }
        do {
            let loaded = try PropertyLoader.load(path)
            details = loaded
            set("Kind", OpenService.typeDescription(forContentType: loaded.contentType, isDirectory: loaded.isDirectory))
            set("Size", loaded.isDirectory ? "Calculating…" : "\(ByteFormat.string(loaded.size)) (\(loaded.size) bytes)")
            set("Where", (path as NSString).deletingLastPathComponent)
            set("Created", loaded.created.map(Formatters.date) ?? "—")
            set("Modified", loaded.modified.map(Formatters.date) ?? "—")
            set("Last opened", loaded.accessed.map(Formatters.date) ?? "—")
            set("Owner", loaded.owner)
            set("Group", loaded.group)
            show(loaded.permissions)
            let mine = NSUserName() == loaded.owner
            checkboxes.flatMap { $0 }.forEach { $0.isEnabled = mine }
            if !mine { status.stringValue = "Only the owner can change permissions." }
            if loaded.isDirectory { computeFolderSize(path) }
        } catch {
            status.stringValue = "Couldn’t read the details of this item."
            for (label, _) in rows { set(label, "Not available") }
            checkboxes.flatMap { $0 }.forEach { $0.isEnabled = false }
        }
    }

    private func set(_ label: String, _ value: String) { rows.first { $0.0 == label }?.1.stringValue = value }

    private func show(_ bits: PermissionBits) {
        let values = [[bits.ownerRead, bits.ownerWrite, bits.ownerExecute], [bits.groupRead, bits.groupWrite, bits.groupExecute], [bits.otherRead, bits.otherWrite, bits.otherExecute]]
        for (row, flags) in values.enumerated() { for (column, flag) in flags.enumerated() { checkboxes[row][column].state = flag ? .on : .off } }
        apply.isEnabled = false
    }

    private func currentBits() -> PermissionBits {
        func on(_ r: Int, _ c: Int) -> Bool { checkboxes[r][c].state == .on }
        var bits = PermissionBits()
        (bits.ownerRead, bits.ownerWrite, bits.ownerExecute) = (on(0, 0), on(0, 1), on(0, 2))
        (bits.groupRead, bits.groupWrite, bits.groupExecute) = (on(1, 0), on(1, 1), on(1, 2))
        (bits.otherRead, bits.otherWrite, bits.otherExecute) = (on(2, 0), on(2, 1), on(2, 2))
        return bits
    }

    @objc private func permissionChanged() { apply.isEnabled = details.map { currentBits() != $0.permissions } ?? false }

    @objc private func applyPermissions() {
        guard let path = item.location.localPath else { return }
        do {
            try PropertyLoader.setPermissions(currentBits(), at: path)
            details = try PropertyLoader.load(path)
            show(details!.permissions)
            status.stringValue = "Permissions updated."
        } catch {
            status.stringValue = "Couldn’t change the permissions."
            if let details { show(details.permissions) }
        }
    }

    /// Totals the folder's contents on a worker so the window opens at once.
    private func computeFolderSize(_ path: String) {
        let token = CancellationToken()
        sizeToken = token
        DispatchQueue.global(qos: .utility).async {
            var total: UInt64 = 0
            var count = 0
            let keys: [URLResourceKey] = [.totalFileAllocatedSizeKey, .isRegularFileKey]
            if let walker = FileManager.default.enumerator(at: URL(fileURLWithPath: path), includingPropertiesForKeys: keys, options: [], errorHandler: { _, _ in true }) {
                for case let url as URL in walker {
                    if token.isCancelled { return }
                    if let values = try? url.resourceValues(forKeys: Set(keys)), values.isRegularFile == true {
                        total += UInt64(values.totalFileAllocatedSize ?? 0)
                        count += 1
                    }
                }
            }
            DispatchQueue.main.async { [weak self] in
                guard !token.isCancelled else { return }
                self?.set("Size", "\(ByteFormat.string(total)) on disk (\(count) file\(count == 1 ? "" : "s"))")
            }
        }
    }
}
