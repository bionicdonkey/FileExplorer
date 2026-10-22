import AppKit
import ExplorerDomain

/// One tab's whole view: the location bar over the files over the status bar, with the folder
/// suggestions floating under the location field while it is being edited.
final class BrowserViewController: NSViewController {
    let tab: TabModel
    let services: AppServices
    let commands: FileCommands
    let content: ContentViewController
    let locationBar = LocationBar()
    let statusBar = StatusBar()
    private let suggestions = SuggestionList()
    private var suggestionHeight: NSLayoutConstraint!
    private var suggestionRequest = 0
    private var directoryCache: [String: [String]] = [:]
    var onTitleChanged: () -> Void = {}
    var onShowOperations: (NSView) -> Void = { _ in }

    init(tab: TabModel, services: AppServices, commands: FileCommands) {
        self.tab = tab
        self.services = services
        self.commands = commands
        content = ContentViewController(tab: tab, services: services, commands: commands)
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        let root = NSView()
        addChild(content)
        for subview in [locationBar, content.view, statusBar, suggestions] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; root.addSubview(subview) }
        suggestionHeight = suggestions.heightAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            locationBar.topAnchor.constraint(equalTo: root.topAnchor), locationBar.leadingAnchor.constraint(equalTo: root.leadingAnchor), locationBar.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            content.view.topAnchor.constraint(equalTo: locationBar.bottomAnchor), content.view.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            content.view.trailingAnchor.constraint(equalTo: root.trailingAnchor), content.view.bottomAnchor.constraint(equalTo: statusBar.topAnchor),
            statusBar.leadingAnchor.constraint(equalTo: root.leadingAnchor), statusBar.trailingAnchor.constraint(equalTo: root.trailingAnchor), statusBar.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            suggestions.topAnchor.constraint(equalTo: locationBar.bottomAnchor, constant: -2), suggestions.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 10),
            suggestions.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -10), suggestionHeight,
            root.widthAnchor.constraint(greaterThanOrEqualToConstant: 420), root.heightAnchor.constraint(greaterThanOrEqualToConstant: 240),
        ])
        view = root
        wire()
        refreshAll()
    }

    // MARK: wiring

    private func wire() {
        tab.onChange = { [weak self] change in self?.tabChanged(change) }
        content.onSelectionChanged = { [weak self] in self?.updateStatus() }
        statusBar.onShowOperations = { [weak self] anchor in self?.onShowOperations(anchor) }
        NotificationCenter.default.addObserver(self, selector: #selector(operationsChanged), name: .operationsChanged, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(elevatedFinished), name: .elevatedOperationFinished, object: nil)
        locationBar.onNavigate = { [weak self] location in self?.tab.navigate(to: location) }
        locationBar.onSubmit = { [weak self] text in self?.submitLocation(text) }
        locationBar.onTextChanged = { [weak self] text in self?.updateSuggestions(for: text) }
        locationBar.onMoveSuggestion = { [weak self] delta in self?.suggestions.move(delta) }
        locationBar.onAcceptSuggestion = { [weak self] in self?.suggestions.candidate?.text }
        locationBar.onSelectedSuggestion = { [weak self] in self?.suggestions.selected?.text }
        locationBar.onEditingEnded = { [weak self] in self?.hideSuggestions() }
        suggestions.onChoose = { [weak self] suggestion in
            self?.locationBar.accept(suggestion.text)
        }
    }

    deinit { NotificationCenter.default.removeObserver(self) }

    @objc private func operationsChanged() {
        statusBar.show(operations: services.operations)
        // A finished operation changes what the watcher may not see (a network or archive folder).
        if !tab.isSearching, services.operations.activeCount == 0, tab.location.localPath == nil { tab.refresh() }
    }

    @objc private func elevatedFinished() { tab.refresh() }

    private func tabChanged(_ change: TabChange) {
        content.apply(change)
        if change.contains(.navigation) { locationBar.show(tab.location); onTitleChanged() }
        updateStatus()
    }

    private func refreshAll() {
        locationBar.show(tab.location)
        statusBar.show(operations: services.operations)
        updateStatus()
    }

    private func updateStatus() { statusBar.show(text: tab.statusText) }

    func focusFiles() { if let target = content.focusView { view.window?.makeFirstResponder(target) } }

    // MARK: location entry

    private func submitLocation(_ text: String) {
        guard !text.isEmpty else { return locationBar.endEditing() }
        var input = text
        if input == "~" || input.hasPrefix("~/") { input = NSHomeDirectory() + input.dropFirst() }
        do {
            let location = try LocationInput.parse(input)
            locationBar.endEditing()
            tab.navigate(to: location)
            focusFiles()
        } catch {
            locationBar.rejectInput("That isn’t a valid path or address.")
        }
    }

    private func updateSuggestions(for text: String) {
        guard let target = CompletionTarget(text.hasPrefix("~") ? NSHomeDirectory() + text.dropFirst() : text) else { return hideSuggestions() }
        suggestionRequest += 1
        let request = suggestionRequest
        if let names = directoryCache[target.directoryText] { return showSuggestions(target.suggestions(from: names)) }
        services.directories.list(DirectoryRequest(location: target.directory)) { [weak self] completion in
            guard let self, request == self.suggestionRequest else { return }
            guard case .succeeded(let snapshot) = completion.outcome else { return self.hideSuggestions() }
            let names = Array(snapshot.entries.filter { $0.kind == .directory }.map(\.displayName).prefix(CompletionTarget.maximumEnumerated))
            self.directoryCache[target.directoryText] = names
            if self.directoryCache.count > 20 { self.directoryCache.removeAll(keepingCapacity: true); self.directoryCache[target.directoryText] = names }
            self.showSuggestions(target.suggestions(from: names))
        }
    }

    private func showSuggestions(_ list: [CompletionTarget.Suggestion]) {
        guard locationBar.isEditing else { return }
        suggestions.show(list)
        suggestionHeight.constant = list.isEmpty ? 0 : suggestions.preferredHeight
    }

    private func hideSuggestions() {
        suggestionRequest += 1
        suggestions.hide()
        suggestionHeight.constant = 0
        directoryCache.removeAll()
    }
}
