import AppKit
import ExplorerDomain

private final class FileTableView: NSTableView {
    var menuProvider: ((Int) -> NSMenu?)?
    var onReturn: (() -> Void)?

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = self.row(at: convert(event.locationInWindow, from: nil))
        return menuProvider?(row)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 { onReturn?() } else { super.keyDown(with: event) }
    }
}

/// The detailed and compact lists: an NSTableView with the four columns, sortable by header,
/// reorderable and resizable, with group rows when grouping is on.
final class FileListView: NSObject, FileView, NSTableViewDataSource, NSTableViewDelegate {
    weak var host: FileViewHost?
    private let scroll = NSScrollView()
    private let table = FileTableView()
    private enum Row { case group(String), item(FileItem) }
    private var rows: [Row] = []
    private var applying = false
    private var layoutSaveWork: DispatchWorkItem?

    var view: NSView { scroll }
    var focusView: NSView { table }

    override init() {
        super.init()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        table.dataSource = self
        table.delegate = self
        table.allowsMultipleSelection = true
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.usesAlternatingRowBackgroundColors = true
        table.style = .fullWidth
        table.columnAutoresizingStyle = .noColumnAutoresizing
        table.target = self
        table.doubleAction = #selector(doubleClicked)
        table.registerForDraggedTypes([.fileURL])
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: false)
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.draggingDestinationFeedbackStyle = .regular
        table.setAccessibilityLabel("Files")
        table.menuProvider = { [weak self] row in self?.menu(forRow: row) }
        table.onReturn = { [weak self] in self?.activateSelection() }
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(boundsChanged), name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        NotificationCenter.default.addObserver(self, selector: #selector(columnsChanged), name: NSTableView.columnDidResizeNotification, object: table)
        NotificationCenter.default.addObserver(self, selector: #selector(columnsChanged), name: NSTableView.columnDidMoveNotification, object: table)
    }

    deinit { NotificationCenter.default.removeObserver(self) }

    private var tab: TabModel? { host?.tab }

    // MARK: reload

    func reload() {
        guard let tab else { return }
        applying = true
        defer { applying = false }
        rows = tab.groups.flatMap { group -> [Row] in
            (group.title.map { [Row.group($0)] } ?? []) + group.items.map(Row.item)
        }
        configureColumns(tab.options)
        let compact = tab.options.viewMode == .compactList
        table.rowHeight = CGFloat(compact ? 20 : max(24, tab.options.gridSize.listIconSize + 8))
        table.reloadData()
        applySelection()
        DispatchQueue.main.async { [weak self] in self?.reportVisible() }
    }

    private func title(for column: ListColumn) -> String {
        switch column { case .name: return "Name"; case .dateModified: return "Date Modified"; case .type: return "Type"; case .size: return "Size" }
    }

    private func configureColumns(_ options: ViewOptions) {
        let layout = options.listColumns.normalized()
        let current = table.tableColumns.compactMap { ListColumn(rawValue: $0.identifier.rawValue) }
        if current != layout.order {
            table.tableColumns.forEach(table.removeTableColumn)
            for column in layout.order {
                let tableColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.rawValue))
                tableColumn.title = title(for: column)
                tableColumn.minWidth = 64
                tableColumn.headerCell.alignment = column == .size ? .right : .left
                table.addTableColumn(tableColumn)
            }
        }
        for tableColumn in table.tableColumns {
            guard let column = ListColumn(rawValue: tableColumn.identifier.rawValue) else { continue }
            let width = CGFloat(layout.width(for: column))
            if abs(tableColumn.width - width) > 0.5 { tableColumn.width = width }
            let sorted = sortField(for: column) == options.sortField
            let image = sorted ? NSImage(named: options.sortDirection == .ascending ? "NSAscendingSortIndicator" : "NSDescendingSortIndicator") : nil
            table.setIndicatorImage(image, in: tableColumn)
        }
    }

    private func sortField(for column: ListColumn) -> SortField {
        switch column { case .name: return .name; case .dateModified: return .dateModified; case .type: return .type; case .size: return .size }
    }

    @objc private func columnsChanged() {
        guard !applying, let tab else { return }
        layoutSaveWork?.cancel()
        let work = DispatchWorkItem { [weak self, weak tab] in
            guard let self, let tab else { return }
            var order: [ListColumn] = []
            var widths: [ListColumn: Int] = [:]
            for column in self.table.tableColumns {
                guard let id = ListColumn(rawValue: column.identifier.rawValue) else { continue }
                order.append(id)
                widths[id] = Int(column.width.rounded())
            }
            tab.updateOptions { $0.listColumns = ListColumnLayout(order: order, widths: widths).normalized() }
        }
        layoutSaveWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: work)
    }

    // MARK: selection

    private func item(at row: Int) -> FileItem? {
        guard rows.indices.contains(row), case .item(let item) = rows[row] else { return nil }
        return item
    }

    private var selectedItems: [FileItem] { table.selectedRowIndexes.compactMap(item(at:)) }

    func applySelection() {
        guard let tab else { return }
        let keys = Set(tab.state.selectionKeys)
        var indexes = IndexSet()
        for (index, row) in rows.enumerated() { if case .item(let item) = row, keys.contains(item.identityKey) { indexes.insert(index) } }
        if indexes != table.selectedRowIndexes {
            applying = true
            table.selectRowIndexes(indexes, byExtendingSelection: false)
            applying = false
        }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !applying else { return }
        host?.fileViewSelectionChanged(selectedItems)
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { item(at: row) != nil }

    func scroll(to item: FileItem) {
        if let row = rows.firstIndex(where: { if case .item(let candidate) = $0 { return candidate.identityKey == item.identityKey } else { return false } }) {
            table.scrollRowToVisible(row)
        }
    }

    // MARK: data

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool { item(at: row) == nil }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch rows[row] {
        case .group(let title):
            let id = NSUserInterfaceItemIdentifier("group")
            let cell = (tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView) ?? makeCell(id, withIcon: false)
            cell.textField?.stringValue = title
            cell.textField?.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
            return cell
        case .item(let item):
            guard let column = tableColumn.flatMap({ ListColumn(rawValue: $0.identifier.rawValue) }) else { return nil }
            let id = NSUserInterfaceItemIdentifier("cell-" + column.rawValue)
            let cell = (tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView) ?? makeCell(id, withIcon: column == .name)
            cell.textField?.font = .systemFont(ofSize: NSFont.systemFontSize)
            cell.textField?.alignment = column == .size ? .right : .left
            switch column {
            case .name:
                cell.textField?.stringValue = host?.tab.displayName(for: item) ?? item.name
                let points = host?.tab.options.viewMode == .compactList ? 16 : (host?.tab.options.gridSize.listIconSize ?? 20)
                cell.imageView?.image = IconCache.shared.icon(for: item, points: points)
                cell.textField?.isEditable = false
            case .dateModified: cell.textField?.stringValue = Formatters.date(item.modified)
            case .type: cell.textField?.stringValue = Formatters.type(item)
            case .size: cell.textField?.stringValue = Formatters.size(item)
            }
            return cell
        }
    }

    private func makeCell(_ id: NSUserInterfaceItemIdentifier, withIcon: Bool) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = id
        let label = NSTextField(labelWithString: "")
        label.lineBreakMode = .byTruncatingMiddle
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        cell.textField = label
        if withIcon {
            let icon = NSImageView()
            icon.translatesAutoresizingMaskIntoConstraints = false
            icon.imageScaling = .scaleProportionallyUpOrDown
            cell.addSubview(icon)
            cell.imageView = icon
            NSLayoutConstraint.activate([
                icon.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                icon.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                icon.heightAnchor.constraint(equalTo: cell.heightAnchor, constant: -4), icon.widthAnchor.constraint(equalTo: icon.heightAnchor),
                label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            ])
            icon.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        } else {
            label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4).isActive = true
        }
        NSLayoutConstraint.activate([
            label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func tableView(_ tableView: NSTableView, didClick tableColumn: NSTableColumn) {
        guard let column = ListColumn(rawValue: tableColumn.identifier.rawValue), let tab else { return }
        let field = sortField(for: column)
        tab.updateOptions {
            if $0.sortField == field { $0.sortDirection = $0.sortDirection == .ascending ? .descending : .ascending }
            else { $0.sortField = field; $0.sortDirection = .ascending }
        }
    }

    func tableView(_ tableView: NSTableView, typeSelectStringFor tableColumn: NSTableColumn?, row: Int) -> String? { item(at: row)?.name }

    /// Rows that scroll into view ask for their size, date and type.
    @objc private func boundsChanged() { reportVisible() }

    private func reportVisible() {
        let range = table.rows(in: table.visibleRect)
        let items = (range.location..<(range.location + range.length)).compactMap(item(at:))
        host?.fileViewVisible(items)
    }

    // MARK: activation and menu

    @objc private func doubleClicked() {
        if let item = item(at: table.clickedRow) { host?.fileViewActivate(item) }
    }

    private func activateSelection() { selectedItems.first.map { host?.fileViewActivate($0) } }

    private func menu(forRow row: Int) -> NSMenu? {
        if row >= 0, item(at: row) != nil, !table.selectedRowIndexes.contains(row) {
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else if row < 0 {
            table.deselectAll(nil)
        }
        return host?.fileViewMenu(for: selectedItems)
    }

    // MARK: rename

    func beginRename(_ item: FileItem) {
        RenamePrompt.ask(for: item, in: table.window) { [weak self] name in self?.host?.fileViewRename(item, to: name) }
    }

    // MARK: drag and drop

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? { item(at: row)?.pasteboardURL }

    func tableView(_ tableView: NSTableView, validateDrop info: NSDraggingInfo, proposedRow row: Int, proposedDropOperation: NSTableView.DropOperation) -> NSDragOperation {
        guard let host else { return [] }
        if proposedDropOperation == .on, let target = item(at: row), target.isDirectory {
            tableView.setDropRow(row, dropOperation: .on)
            return DropSupport.validate(info, host: host, destination: target.location)
        }
        tableView.setDropRow(-1, dropOperation: .on)
        return DropSupport.validate(info, host: host, destination: host.tab.location)
    }

    func tableView(_ tableView: NSTableView, acceptDrop info: NSDraggingInfo, row: Int, dropOperation: NSTableView.DropOperation) -> Bool {
        guard let host else { return false }
        if dropOperation == .on, let target = item(at: row), target.isDirectory {
            return DropSupport.accept(info, host: host, destination: target.location)
        }
        return DropSupport.accept(info, host: host, destination: host.tab.location)
    }
}
