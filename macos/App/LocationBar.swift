import AppKit
import ExplorerDomain

/// The path above the files: a breadcrumb of the current location whose parts navigate, and an
/// editable text field (click the empty space, or press ⌘L) that takes a path or a URI and suggests
/// folders while typing. The field never navigates by itself; it reports what was typed.
final class LocationBar: NSView, NSTextFieldDelegate {
    var onNavigate: (Location) -> Void = { _ in }
    var onSubmit: (String) -> Void = { _ in }
    var onTextChanged: (String) -> Void = { _ in }
    var onMoveSuggestion: (Int) -> Void = { _ in }
    var onAcceptSuggestion: () -> String? = { nil }
    var onSelectedSuggestion: () -> String? = { nil }
    var onEditingEnded: () -> Void = {}

    private let crumbs = NSStackView()
    private let field = NSTextField()
    private let box = NSView()
    private let clickArea = NSClickGestureRecognizer()
    private(set) var isEditing = false
    private var location = Location.home

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        box.wantsLayer = true
        box.layer?.cornerRadius = 6
        box.layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
        box.layer?.borderColor = NSColor.separatorColor.cgColor
        box.layer?.borderWidth = 1
        crumbs.orientation = .horizontal
        crumbs.spacing = 2
        crumbs.alignment = .centerY
        field.isHidden = true
        field.delegate = self
        field.placeholderString = "Path or URL"
        field.font = .systemFont(ofSize: NSFont.systemFontSize)
        field.focusRingType = .none
        field.isBordered = false
        field.drawsBackground = false
        field.setAccessibilityLabel("Location")
        field.setAccessibilityIdentifier("location-field")
        crumbs.setAccessibilityLabel("Path")
        for subview in [box, crumbs, field] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; addSubview(subview) }
        NSLayoutConstraint.activate([
            box.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10), box.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            box.centerYAnchor.constraint(equalTo: centerYAnchor), box.heightAnchor.constraint(equalToConstant: 26),
            crumbs.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 6), crumbs.centerYAnchor.constraint(equalTo: box.centerYAnchor),
            crumbs.trailingAnchor.constraint(lessThanOrEqualTo: box.trailingAnchor, constant: -6),
            field.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 8), field.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -8),
            field.centerYAnchor.constraint(equalTo: box.centerYAnchor),
            heightAnchor.constraint(equalToConstant: 36),
        ])
        clickArea.target = self
        clickArea.action = #selector(clickedBackground)
        box.addGestureRecognizer(clickArea)
        crumbs.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }

    required init?(coder: NSCoder) { fatalError() }

    // MARK: breadcrumb

    func show(_ location: Location) {
        self.location = location
        if isEditing { field.stringValue = Self.text(for: location) }
        crumbs.arrangedSubviews.forEach { $0.removeFromSuperview() }
        var chain: [Location] = []
        var cursor: Location? = location
        while let current = cursor, chain.count < 64 {
            chain.insert(current, at: 0)
            cursor = current.isInsideArchive && current.parent == current ? nil : current.parent
            if let next = cursor, chain.contains(next) { break }
        }
        let limit = 6
        if chain.count > limit {
            let hidden = chain[0..<(chain.count - limit)]
            chain = Array(chain.suffix(limit))
            let more = NSButton(title: "…", target: self, action: #selector(showHidden(_:)))
            style(more)
            more.tag = hidden.count
            more.toolTip = "Show parent folders"
            more.setAccessibilityLabel("More parent folders")
            crumbs.addArrangedSubview(more)
            hiddenChain = Array(hidden)
        } else {
            hiddenChain = []
        }
        for (index, part) in chain.enumerated() {
            if index > 0 || !hiddenChain.isEmpty { crumbs.addArrangedSubview(chevron()) }
            let name = part.localPath == "/" ? "Macintosh HD" : part.displayName
            let button = NSButton(title: name, target: self, action: #selector(crumbClicked(_:)))
            style(button)
            button.tag = index
            button.setAccessibilityLabel(name)
            if index == chain.count - 1 { button.font = .boldSystemFont(ofSize: NSFont.systemFontSize) }
            crumbs.addArrangedSubview(button)
        }
        shownChain = chain
    }

    private var shownChain: [Location] = []
    private var hiddenChain: [Location] = []

    private func style(_ button: NSButton) {
        button.isBordered = false
        button.font = .systemFont(ofSize: NSFont.systemFontSize)
        button.lineBreakMode = .byTruncatingMiddle
        button.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }

    private func chevron() -> NSView {
        let view = NSImageView(image: Symbol.image("chevron.right", description: nil) ?? NSImage())
        view.contentTintColor = .tertiaryLabelColor
        view.setAccessibilityElement(false)
        return view
    }

    @objc private func crumbClicked(_ sender: NSButton) {
        guard shownChain.indices.contains(sender.tag) else { return }
        onNavigate(shownChain[sender.tag])
    }

    @objc private func showHidden(_ sender: NSButton) {
        let menu = NSMenu()
        for part in hiddenChain.reversed() {
            let item = NSMenuItem(title: part.localPath == "/" ? "Macintosh HD" : part.displayName, action: #selector(menuNavigate(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = part.uri
            menu.addItem(item)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height), in: sender)
    }

    @objc private func menuNavigate(_ sender: NSMenuItem) {
        if let uri = sender.representedObject as? String, let location = try? Location(uri: uri) { onNavigate(location) }
    }

    // MARK: editing

    static func text(for location: Location) -> String { location.localPath ?? location.uri }

    @objc private func clickedBackground() { beginEditing() }

    func beginEditing() {
        guard !isEditing else { return field.selectText(nil) }
        isEditing = true
        crumbs.isHidden = true
        field.isHidden = false
        field.stringValue = Self.text(for: location)
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
        box.layer?.borderColor = NSColor.controlAccentColor.cgColor
    }

    func endEditing() {
        guard isEditing else { return }
        isEditing = false
        field.isHidden = true
        crumbs.isHidden = false
        box.layer?.borderColor = NSColor.separatorColor.cgColor
        field.toolTip = nil
        onEditingEnded()
    }

    func controlTextDidChange(_ notification: Notification) {
        field.toolTip = nil
        onTextChanged(field.stringValue)
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        // Leaving the field by clicking elsewhere abandons the edit; Return is handled in doCommandBy.
        DispatchQueue.main.async { [weak self] in
            guard let self, self.isEditing, self.window?.firstResponder !== self.field.currentEditor() else { return }
            self.endEditing()
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            if let chosen = onSelectedSuggestion() { accept(chosen); return true }
            let text = field.stringValue.trimmingCharacters(in: .whitespaces)
            onSubmit(text)
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            endEditing()
            return true
        case #selector(NSResponder.moveDown(_:)): onMoveSuggestion(1); return true
        case #selector(NSResponder.moveUp(_:)): onMoveSuggestion(-1); return true
        case #selector(NSResponder.insertTab(_:)):
            if let text = onAcceptSuggestion() {
                field.stringValue = text
                field.currentEditor()?.selectedRange = NSRange(location: (text as NSString).length, length: 0)
                onTextChanged(text)
            }
            return true
        default: return false
        }
    }

    /// Replaces the text with a chosen suggestion and keeps editing, so the next folder can follow.
    func accept(_ text: String) {
        field.stringValue = text
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectedRange = NSRange(location: (text as NSString).length, length: 0)
        onTextChanged(text)
    }

    func rejectInput(_ message: String) {
        field.toolTip = message
        NSSound.beep()
        box.layer?.borderColor = NSColor.systemRed.cgColor
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) { [weak self] in
            guard let self else { return }
            self.box.layer?.borderColor = (self.isEditing ? NSColor.controlAccentColor : NSColor.separatorColor).cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        box.layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
        box.layer?.borderColor = (isEditing ? NSColor.controlAccentColor : NSColor.separatorColor).cgColor
    }
}

/// A list of folder suggestions under the location field. It never takes keyboard focus; the field
/// moves the selection with the arrow keys.
final class SuggestionList: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onChoose: (CompletionTarget.Suggestion) -> Void = { _ in }
    private let scroll = NSScrollView()
    private let table = NSTableView()
    private(set) var suggestions: [CompletionTarget.Suggestion] = []

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 6
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("suggestion"))
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 22
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(clicked)
        table.setAccessibilityLabel("Folder suggestions")
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        NSLayoutConstraint.activate([scroll.topAnchor.constraint(equalTo: topAnchor), scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
                                     scroll.leadingAnchor.constraint(equalTo: leadingAnchor), scroll.trailingAnchor.constraint(equalTo: trailingAnchor)])
        isHidden = true
    }

    required init?(coder: NSCoder) { fatalError() }

    var selected: CompletionTarget.Suggestion? {
        table.selectedRow >= 0 ? suggestions[table.selectedRow] : nil
    }

    /// The suggestion Tab would take: the highlighted one, or the only one.
    var candidate: CompletionTarget.Suggestion? { selected ?? (suggestions.count == 1 ? suggestions[0] : suggestions.first) }

    func show(_ new: [CompletionTarget.Suggestion]) {
        suggestions = new
        table.reloadData()
        isHidden = new.isEmpty
    }

    func hide() { suggestions = []; isHidden = true }

    func move(_ delta: Int) {
        guard !suggestions.isEmpty else { return }
        let next = min(max(table.selectedRow + delta, -1), suggestions.count - 1)
        if next < 0 { table.deselectAll(nil) } else { table.selectRowIndexes(IndexSet(integer: next), byExtendingSelection: false); table.scrollRowToVisible(next) }
    }

    @objc private func clicked() { if table.clickedRow >= 0 { onChoose(suggestions[table.clickedRow]) } }

    func numberOfRows(in tableView: NSTableView) -> Int { suggestions.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let label = NSTextField(labelWithString: suggestions[row].name)
        label.lineBreakMode = .byTruncatingMiddle
        let image = NSImageView(image: Symbol.image("folder", description: nil) ?? NSImage())
        let cell = NSView()
        for subview in [image, label] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; cell.addSubview(subview) }
        NSLayoutConstraint.activate([image.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 8), image.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                                     label.leadingAnchor.constraint(equalTo: image.trailingAnchor, constant: 6), label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -8),
                                     label.centerYAnchor.constraint(equalTo: cell.centerYAnchor)])
        return cell
    }

    var preferredHeight: CGFloat { CGFloat(min(suggestions.count, 8)) * 22 + 4 }
}
