import AppKit
import ExplorerAdapters
import ExplorerDomain

/// What changed in a tab, so a view reloads only what it must.
struct TabChange: OptionSet {
    let rawValue: Int
    static let navigation = TabChange(rawValue: 1)      // location, history, status, title
    static let items = TabChange(rawValue: 2)           // the rows to show
    static let selection = TabChange(rawValue: 4)
    static let options = TabChange(rawValue: 8)         // view mode, sorting, grouping
    static let search = TabChange(rawValue: 16)
    static let all: TabChange = [.navigation, .items, .selection, .options, .search]
}

/// One tab: its navigation and search state machines, the items it shows (sorted, filtered and
/// grouped), its view options and its selection. Views observe `onChange` and never hold domain state.
final class TabModel {
    let id = UUID()
    let services: AppServices
    let state: TabState
    let navigation: NavigationCoordinator
    let search: SearchCoordinator
    var onChange: ((TabChange) -> Void)?
    /// Asks the view to start renaming this item (from the menu or the keyboard).
    var onRenameRequested: ((FileItem) -> Void)?

    private(set) var options = ViewOptions()
    /// Items in display order, as groups (one untitled group when not grouped).
    private(set) var groups: [(title: String?, items: [FileItem])] = [(nil, [])]
    private(set) var displayed: [FileItem] = []
    private(set) var isSearching = false
    private(set) var searchText = ""
    private(set) var searchMode = SearchMode.filename
    private var metadata: [String: VisibleMetadata] = [:]
    private var requestedMetadata = Set<String>()
    private var watch: WatchHandle?
    private var watchedLocation: Location?
    private var refreshWork: DispatchWorkItem?
    private var rebuildWork: DispatchWorkItem?
    private var metadataRequests: [CancellationToken] = []

    init(location: Location, services: AppServices = .shared) {
        self.services = services
        state = TabState(location: location)
        navigation = NavigationCoordinator(state: state, repository: services.directories)
        search = SearchCoordinator(tab: state, repository: services.searchRepository)
    }

    deinit { watch?.cancel(); cancelMetadata() }

    // MARK: navigation

    var location: Location { state.location }
    var title: String { state.location.displayName }
    var canGoBack: Bool { state.history.canGoBack }
    var canGoForward: Bool { state.history.canGoForward }
    var parent: Location? { state.location.parent }
    var capabilities: CapabilitySnapshot { state.directory?.capabilities ?? CapabilitySnapshot() }

    /// What to say when a location could not be shown, or nil.
    var failureMessage: String? {
        guard case .failed(let location, let error) = state.status else { return nil }
        return "Can’t open “\(location.displayName)”: " + Self.describe(error.kind)
    }

    var isLoading: Bool { if case .loading = state.status { return true } else { return false } }

    static func describe(_ kind: OperationErrorKind) -> String {
        switch kind {
        case .permissionDenied: return "you don’t have permission to view it."
        case .notFound: return "it no longer exists."
        case .mountUnavailable: return "its volume is not available."
        case .unsupported: return "this kind of location is not supported."
        case .authenticationRequired: return "it needs credentials."
        default: return "something went wrong."
        }
    }

    func navigate(to location: Location) {
        endSearch(notify: false)
        navigation.navigate(to: location) { [weak self] _ in self?.listingChanged() }
        notify([.navigation])
    }

    func back() { endSearch(notify: false); navigation.back { [weak self] _ in self?.listingChanged() }; notify([.navigation]) }
    func forward() { endSearch(notify: false); navigation.forward { [weak self] _ in self?.listingChanged() }; notify([.navigation]) }
    func up() { if let parent { navigate(to: parent) } }
    func refresh() { endSearch(notify: false); navigation.refresh { [weak self] _ in self?.listingChanged() }; notify([.navigation]) }
    func retry() { navigation.retry { [weak self] _ in self?.listingChanged() }; notify([.navigation]) }
    func close() { navigation.closeTab(); watch?.cancel(); watch = nil }

    /// A removed volume invalidates its listing, never the tab's identity or history.
    func volumeUnavailable(_ root: Location) {
        if state.markUnavailable(root: root) { rebuild(); notify([.navigation, .items]) }
    }

    /// Opens an item the way activation says: a folder or archive in this window, a file in its application.
    func open(_ item: FileItem) {
        switch activation(for: item) {
        case .browse(let location): navigate(to: location)
        case .launchExternally: _ = OpenService.open(item.location)
        }
    }

    // MARK: listing

    private func listingChanged() {
        // Streaming pages arrive in a burst; coalesce them so the view reloads a few times, not once per page.
        rebuildWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.listingSettled() }
        rebuildWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.04, execute: work)
    }

    private func listingSettled() {
        if let directory = state.directory, state.hasAvailableDirectory, !isSearching {
            let path = state.location.localPath
            // A folder's own options apply once its items are known (a pictures folder opens as a grid).
            let loaded = directory.entries.map(FileItem.init(snapshot:))
            options = path.map { services.viewPreferences.options(for: $0, items: loaded) } ?? services.viewPreferences.globalDefaults
        }
        if watchedLocation != state.location { startWatching() }
        rebuild()
        notify([.navigation, .items, .options])
        loadMetadataIfSorting()
    }

    private func startWatching() {
        watch?.cancel()
        watchedLocation = state.location
        watch = navigation.watch(state.location) { [weak self] in
            guard let self else { return }
            self.refreshWork?.cancel()
            let work = DispatchWorkItem { [weak self] in
                guard let self, !self.isSearching, !self.isLoading else { return }
                self.navigation.refresh { [weak self] _ in self?.listingChanged() }
            }
            self.refreshWork = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: work)
        }
    }

    private var sourceItems: [FileItem] {
        if case .search(_, let items) = state.content { return items.map(merged) }
        // While a search is running the folder's own items would be misleading next to "Searching…".
        if isSearching { return [] }
        return (state.directory?.entries ?? []).map { merged(FileItem(snapshot: $0)) }
    }

    private func merged(_ item: FileItem) -> FileItem {
        metadata[item.identityKey].map(item.withVisibleMetadata) ?? item
    }

    func rebuild() {
        displayed = DirectoryModel.visible(sourceItems, options: options)
        groups = DirectoryModel.grouped(displayed, options: options, typeName: { OpenService.typeDescription(forContentType: $0.contentType, isDirectory: $0.isDirectory) })
    }

    // MARK: visible metadata

    /// Size, date and content type are fetched for the rows that are on screen (and for every row
    /// when the sort order needs them), never as part of the listing.
    func requestMetadata(for items: [FileItem]) {
        let wanted = items.filter { !$0.contentTypeResolved && !requestedMetadata.contains($0.identityKey) }
        guard !wanted.isEmpty else { return }
        wanted.forEach { requestedMetadata.insert($0.identityKey) }
        let token = CancellationToken()
        metadataRequests.append(token)
        navigation.inspectVisibleMetadata(VisibleMetadataRequest(locations: wanted.map(\.location), cancellation: token)) { [weak self] completion in
            guard let self, case .succeeded(let rows) = completion.outcome else { return }
            for row in rows { self.metadata[row.location.uri] = row }
            self.rebuild()
            self.notify([.items])
        }
    }

    private func loadMetadataIfSorting() {
        guard [.size, .dateModified, .type].contains(options.sortField) || options.groupField != .none else { return }
        let items = sourceItems
        for start in stride(from: 0, to: min(items.count, 20_000), by: 512) {
            requestMetadata(for: Array(items[start..<min(start + 512, items.count)]))
        }
    }

    private func cancelMetadata() { metadataRequests.forEach { $0.cancel() }; metadataRequests.removeAll() }

    // MARK: options

    func setOptions(_ new: ViewOptions, forFolder: Bool = true) {
        options = new
        if forFolder, let path = state.location.localPath {
            services.viewPreferences.set(new, for: path)
            try? services.viewPreferences.save()
        }
        rebuild()
        notify([.options, .items])
        loadMetadataIfSorting()
    }

    func updateOptions(_ change: (inout ViewOptions) -> Void) {
        var copy = options
        change(&copy)
        setOptions(copy)
    }

    // MARK: selection

    var selectedItems: [FileItem] {
        let keys = Set(state.selectionKeys)
        return displayed.filter { keys.contains($0.identityKey) }
    }

    func setSelection(_ items: [FileItem]) {
        state.selectionKeys = items.map(\.identityKey)
        notify([.selection])
    }

    var availability: CommandAvailability {
        CommandAvailability(selection: selectedItems, destinationCapabilities: isSearching ? nil : capabilities)
    }

    // MARK: search

    func beginSearch(_ text: String, mode: SearchMode) {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return endSearch() }
        isSearching = true
        searchText = trimmed
        searchMode = mode
        cancelMetadata()
        let root = underlyingLocation
        var scope = SearchScope.localFallback(root: root)
        // Search the whole tree under the folder the user is in, within sensible limits.
        scope.maxDepth = 8
        scope.timeout = mode == .content ? 10 : 5
        search.submitDebounced(trimmed, mode: mode, scope: scope) { [weak self] _ in
            guard let self else { return }
            self.rebuild()
            self.notify([.items, .search])
        }
        notify([.search])
    }

    /// The folder a search runs in: the one under the overlay, if search is already showing.
    private var underlyingLocation: Location {
        if case .search(let underlying, _) = state.content { return underlying }
        return state.location
    }

    var searchOutcome: SearchOutcome? { search.outcome }

    /// What to call an item in the list: its name, or while searching its path under the searched folder.
    func displayName(for item: FileItem) -> String {
        guard isSearching, let root = underlyingLocation.localPath, let path = item.location.localPath, path.hasPrefix(root + "/") else { return item.name }
        return String(path.dropFirst(root.count + 1))
    }

    func retrySearch() { search.retry { [weak self] _ in self?.rebuild(); self?.notify([.items, .search]) }; notify([.search]) }

    func endSearch(notify shouldNotify: Bool = true) {
        guard isSearching else { return }
        isSearching = false
        searchText = ""
        search.clear()
        rebuild()
        if shouldNotify { notify([.items, .search, .selection]) }
    }

    /// Records a submitted search term, subject to the privacy preference.
    func recordSearchTerm(_ text: String) {
        let prefs = services.preferences.preferences
        try? services.searchHistory.recordSubmitted(text, enabled: prefs.recordSearchHistory, retentionDays: prefs.searchHistoryRetentionDays)
        try? services.searchHistory.save()
    }

    // MARK: status

    var statusText: String {
        if isLoading && displayed.isEmpty { return "Loading…" }
        let total = displayed.count
        let selected = selectedItems
        var text = "\(total) item\(total == 1 ? "" : "s")"
        if isSearching { text = searchSummary(total) }
        if !selected.isEmpty {
            let bytes = selected.filter { !$0.isDirectory }.reduce(UInt64(0)) { $0 + $1.size }
            text += " · \(selected.count) selected" + (bytes > 0 ? " (\(ByteFormat.string(bytes)))" : "")
        }
        return text
    }

    private func searchSummary(_ total: Int) -> String {
        switch search.outcome {
        case nil: return "Searching…"
        case .unsupported?: return "Search is not available here"
        case .incomplete?: return "\(total) match\(total == 1 ? "" : "es") (incomplete)"
        case .completed(_, _, _, let limits)?:
            let limited = limits.contains(.resultCount) && total >= 5000
            return "\(total) match\(total == 1 ? "" : "es")" + (limited ? " (limit reached)" : "")
        }
    }

    private func notify(_ change: TabChange) { onChange?(change) }
}
