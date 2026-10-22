import AppKit
import ExplorerDomain

/// The operations page: every running and recent operation, with progress, cancel and clear.
final class OperationsViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    private let services: AppServices
    private let table = NSTableView()
    private let empty = NSTextField(labelWithString: "No operations yet")
    private let clear = NSButton(title: "Clear Finished", target: nil, action: nil)
    private var observer: NSObjectProtocol?
    private var records: [OperationRecord] = []

    init(services: AppServices) {
        self.services = services
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    deinit { observer.map(NotificationCenter.default.removeObserver) }

    override func loadView() {
        let root = NSView(frame: NSRect(x: 0, y: 0, width: 440, height: 320))
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("operation"))
        column.width = 420
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 54
        table.dataSource = self
        table.delegate = self
        table.selectionHighlightStyle = .none
        table.setAccessibilityLabel("Operations")
        clear.target = self
        clear.action = #selector(clearFinished)
        clear.bezelStyle = .rounded
        empty.textColor = .secondaryLabelColor
        for subview in [scroll, clear, empty] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; root.addSubview(subview) }
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: root.topAnchor), scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor), scroll.bottomAnchor.constraint(equalTo: clear.topAnchor, constant: -8),
            clear.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12), clear.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -10),
            empty.centerXAnchor.constraint(equalTo: root.centerXAnchor), empty.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            root.widthAnchor.constraint(greaterThanOrEqualToConstant: 360), root.heightAnchor.constraint(greaterThanOrEqualToConstant: 220),
        ])
        view = root
        observer = NotificationCenter.default.addObserver(forName: .operationsChanged, object: nil, queue: .main) { [weak self] _ in self?.refresh() }
        refresh()
    }

    private func refresh() {
        records = services.operations.records.filter { !$0.hidden }.reversed()
        table.reloadData()
        empty.isHidden = !records.isEmpty
        clear.isEnabled = records.contains { !$0.state.isActive }
    }

    @objc private func clearFinished() { services.operations.clearCompleted() }

    @objc private func cancel(_ sender: NSButton) { services.operations.cancel(UInt64(sender.tag)) }

    func numberOfRows(in tableView: NSTableView) -> Int { records.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let record = records[row]
        let cell = NSView()
        let title = NSTextField(labelWithString: Self.title(for: record))
        title.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        title.lineBreakMode = .byTruncatingMiddle
        let status = NSTextField(labelWithString: Self.status(for: record))
        status.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        let stack = NSStackView(views: [title, status])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 2
        stack.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(stack)
        var constraints = [stack.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 12), stack.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                           stack.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor, constant: -60)]
        if record.state.isActive {
            let bar = NSProgressIndicator()
            bar.style = .bar
            bar.isIndeterminate = record.total == 0
            bar.minValue = 0
            bar.maxValue = Double(max(record.total, 1))
            bar.doubleValue = Double(record.completed)
            if bar.isIndeterminate { bar.startAnimation(nil) }
            bar.translatesAutoresizingMaskIntoConstraints = false
            bar.setAccessibilityLabel("Progress")
            cell.addSubview(bar)
            let stop = NSButton(image: Symbol.image("xmark.circle.fill", description: "Cancel") ?? NSImage(), target: self, action: #selector(cancel(_:)))
            stop.isBordered = false
            stop.tag = Int(record.id)
            stop.translatesAutoresizingMaskIntoConstraints = false
            stop.setAccessibilityLabel("Cancel \(Messages.title(for: record.displayKind.rawValue))")
            cell.addSubview(stop)
            constraints += [bar.leadingAnchor.constraint(equalTo: stack.leadingAnchor), bar.trailingAnchor.constraint(equalTo: stop.leadingAnchor, constant: -8),
                            bar.topAnchor.constraint(equalTo: stack.bottomAnchor, constant: 4),
                            stop.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -12), stop.centerYAnchor.constraint(equalTo: cell.centerYAnchor)]
            constraints.append(stack.topAnchor.constraint(equalTo: cell.topAnchor, constant: 6))
            constraints.removeAll { $0.firstAttribute == .centerY && ($0.firstItem as? NSStackView) === stack }
        }
        NSLayoutConstraint.activate(constraints)
        return cell
    }

    static func title(for record: OperationRecord) -> String {
        let names = record.sourceURIs.compactMap { try? Location(uri: $0).displayName }
        let subject = names.count == 1 ? "“\(names[0])”" : "\(record.sourceURIs.count) items"
        return "\(Messages.title(for: record.displayKind.rawValue)) \(subject)"
    }

    static func status(for record: OperationRecord) -> String {
        switch record.state {
        case .queued: return "Waiting…"
        case .running: return record.total > 0 ? "\(record.completed) of \(record.total)" : "Working…"
        case .cancelRequested: return "Cancelling…"
        case .completed: return "Done"
        case .cancelled: return "Cancelled"
        case .failed(let key): return Messages.text(forKey: key)
        }
    }
}
