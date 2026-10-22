import AppKit
import ExplorerDomain

/// The area below the location bar: the file view for the current view mode, with a banner for
/// failures and incomplete searches and a message when there is nothing to show.
final class ContentViewController: NSViewController, FileViewHost {
    let tab: TabModel
    let services: AppServices
    let commands: FileCommands
    private let container = NSView()
    private let banner = NSView()
    private let bannerLabel = NSTextField(labelWithString: "")
    private let bannerButton = NSButton(title: "Try Again", target: nil, action: nil)
    private let message = NSTextField(labelWithString: "")
    private let spinner = NSProgressIndicator()
    private var bannerHeight: NSLayoutConstraint!
    private var current: FileView?
    private var currentMode: ViewMode?
    var onSelectionChanged: (() -> Void)?

    init(tab: TabModel, services: AppServices, commands: FileCommands) {
        self.tab = tab
        self.services = services
        self.commands = commands
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        let root = NSView()
        banner.wantsLayer = true
        banner.layer?.backgroundColor = NSColor.systemYellow.withAlphaComponent(0.18).cgColor
        bannerLabel.lineBreakMode = .byTruncatingTail
        bannerButton.bezelStyle = .rounded
        bannerButton.target = self
        bannerButton.action = #selector(bannerAction)
        message.font = .systemFont(ofSize: 15)
        message.textColor = .secondaryLabelColor
        message.alignment = .center
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        for subview in [banner, container, message, spinner] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; root.addSubview(subview) }
        for subview in [bannerLabel, bannerButton] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; banner.addSubview(subview) }
        bannerHeight = banner.heightAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            banner.topAnchor.constraint(equalTo: root.topAnchor), banner.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            banner.trailingAnchor.constraint(equalTo: root.trailingAnchor), bannerHeight,
            bannerLabel.leadingAnchor.constraint(equalTo: banner.leadingAnchor, constant: 12), bannerLabel.centerYAnchor.constraint(equalTo: banner.centerYAnchor),
            bannerButton.trailingAnchor.constraint(equalTo: banner.trailingAnchor, constant: -12), bannerButton.centerYAnchor.constraint(equalTo: banner.centerYAnchor),
            bannerLabel.trailingAnchor.constraint(lessThanOrEqualTo: bannerButton.leadingAnchor, constant: -8),
            container.topAnchor.constraint(equalTo: banner.bottomAnchor), container.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            container.trailingAnchor.constraint(equalTo: root.trailingAnchor), container.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            message.centerXAnchor.constraint(equalTo: container.centerXAnchor), message.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            message.leadingAnchor.constraint(greaterThanOrEqualTo: container.leadingAnchor, constant: 20),
            spinner.centerXAnchor.constraint(equalTo: container.centerXAnchor), spinner.topAnchor.constraint(equalTo: message.bottomAnchor, constant: 8),
        ])
        view = root
        tab.onRenameRequested = { [weak self] item in self?.current?.beginRename(item) }
        apply(.all)
    }

    var focusView: NSView? { current?.focusView }

    // MARK: updates

    func apply(_ change: TabChange) {
        if change.contains(.options) || change.contains(.items) || current == nil { showFileView(reload: true) }
        else if change.contains(.selection) { current?.applySelection() }
        updateBanner()
        updateMessage()
    }

    private func showFileView(reload: Bool) {
        let mode = tab.options.viewMode
        let wantsList = mode == .detailedList || mode == .compactList
        let haveList = currentMode.map { $0 == .detailedList || $0 == .compactList }
        if current == nil || haveList != wantsList {
            current?.view.removeFromSuperview()
            let next: FileView = wantsList ? FileListView() : FileGridView(services: services)
            next.host = self
            next.view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(next.view)
            NSLayoutConstraint.activate([
                next.view.topAnchor.constraint(equalTo: container.topAnchor), next.view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                next.view.leadingAnchor.constraint(equalTo: container.leadingAnchor), next.view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
            current = next
            if view.window?.firstResponder === view.window { view.window?.makeFirstResponder(next.focusView) }
        }
        currentMode = mode
        if reload { current?.reload() }
    }

    private func updateBanner() {
        var text: String?
        var button: String?
        if let failure = tab.failureMessage { text = failure; button = "Try Again" }
        else if tab.isSearching, case .incomplete(_, let reason, _, _, _)? = tab.searchOutcome {
            text = "Search results are incomplete (\(reason.rawValue.replacingOccurrences(of: "_", with: " ")))."
            button = "Try Again"
        } else if tab.isSearching, case .unsupported? = tab.searchOutcome {
            text = "Search isn’t available in this location."
        } else if tab.isSearching, tab.searchMode == .content, !tab.services.preferences.preferences.contentSearchEnabled {
            text = "Searching file contents is turned off in Settings."
        }
        bannerLabel.stringValue = text ?? ""
        bannerButton.isHidden = button == nil
        bannerHeight.constant = text == nil ? 0 : 36
        banner.isHidden = text == nil
    }

    private func updateMessage() {
        var text = ""
        if tab.failureMessage != nil && tab.displayed.isEmpty { text = "" }
        else if tab.isLoading && tab.displayed.isEmpty { text = "Loading…" }
        else if tab.displayed.isEmpty && tab.state.hasAvailableDirectory { text = tab.isSearching ? "No matches" : "This folder is empty" }
        else if tab.displayed.isEmpty && tab.isSearching { text = "Searching…" }
        message.stringValue = text
        message.isHidden = text.isEmpty
        if tab.isLoading && tab.displayed.isEmpty { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
        message.setAccessibilityLabel(text)
    }

    @objc private func bannerAction() {
        if tab.isSearching { tab.retrySearch() } else { tab.retry() }
    }

    // MARK: FileViewHost

    func fileViewSelectionChanged(_ items: [FileItem]) {
        tab.setSelection(items)
        onSelectionChanged?()
    }

    func fileViewActivate(_ item: FileItem) { commands.open([item]) }

    func fileViewVisible(_ items: [FileItem]) { tab.requestMetadata(for: items) }

    func fileViewMenu(for items: [FileItem]) -> NSMenu {
        CommandMenuBuilder.menu(items: items, tab: tab, services: services, commands: commands, target: self, action: #selector(runMenuCommand(_:)))
    }

    @objc private func runMenuCommand(_ sender: NSMenuItem) {
        guard let item = sender as? CommandMenuItem else { return }
        commands.perform(item.command, items: item.items, shiftHeld: NSEvent.modifierFlags.contains(.shift))
    }

    func fileViewCanDrop(sources: [Location], onto destination: Location) -> Bool {
        let caps = destination == tab.location ? tab.capabilities : CapabilitySnapshot(supported: [.create, .list])
        guard !tab.isSearching || destination != tab.location else { return false }
        return caps.supports(.create) && !transferSources(sources, destination: destination, action: .copy).isEmpty
    }

    func fileViewDrop(sources: [Location], onto destination: Location, copy: Bool) -> Bool {
        commands.transfer(sources, to: destination, copy: copy)
    }

    func fileViewRename(_ item: FileItem, to name: String) { commands.rename(item, to: name) }
}
