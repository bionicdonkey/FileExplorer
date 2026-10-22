import Foundation

/// Back and forward history for one tab.
public struct NavigationHistory {
    public static let maximumRecords = 100
    private(set) var entries: [Location] = []
    private(set) var cursor: Int?

    public init() {}

    public mutating func push(_ location: Location) {
        if current == location { return }
        if let cursor { entries.removeSubrange((cursor + 1)...) }
        entries.append(location)
        if entries.count > NavigationHistory.maximumRecords { entries.removeFirst() }
        cursor = entries.count - 1
    }

    @discardableResult
    public mutating func back() -> Location? {
        guard let cursor, cursor > 0 else { return nil }
        self.cursor = cursor - 1
        return current
    }

    @discardableResult
    public mutating func forward() -> Location? {
        guard let cursor, cursor + 1 < entries.count else { return nil }
        self.cursor = cursor + 1
        return current
    }

    public var current: Location? { cursor.flatMap { entries.indices.contains($0) ? entries[$0] : nil } }
    public var backTarget: Location? { cursor.flatMap { $0 >= 1 ? entries[$0 - 1] : nil } }
    public var forwardTarget: Location? { cursor.flatMap { $0 + 1 < entries.count ? entries[$0 + 1] : nil } }
    public var canGoBack: Bool { (cursor ?? 0) > 0 }
    public var canGoForward: Bool { cursor.map { $0 + 1 < entries.count } ?? false }

    /// Replaces the entry at the cursor with the location a provider resolved to.
    mutating func replaceCurrent(_ location: Location) {
        if let cursor { entries[cursor] = location }
    }
}

public enum TabContent {
    case directory(Location)
    case sectionOverview(section: SidebarSection, underlying: Location)
    case search(underlying: Location, items: [FileItem])
    case unavailable(Location)
}

public enum NavigationStatus {
    case idle
    case loading(Location)
    case ready
    case failed(location: Location, error: OperationError)
    case closed
}

enum HistoryCommit { case push, replace, back, forward }

public struct NavigationRequest {
    public let generation: UInt64
    public var request: DirectoryRequest
    var commit: HistoryCommit
}

/// Main-thread tab state. A pending request never replaces the successful view: the newest
/// request wins, and history commits only on success.
public final class TabState {
    public private(set) var location: Location
    public private(set) var history = NavigationHistory()
    public internal(set) var content: TabContent
    public private(set) var navigationGeneration: UInt64 = 0
    public var selectionKeys: [String] = []
    public private(set) var directory: DirectorySnapshot?
    public private(set) var status: NavigationStatus = .idle
    private var pending: NavigationRequest?
    private var retry: (Location, HistoryCommit)?
    private var pageKeys = Set<Location>()

    public init(location: Location) {
        self.location = location
        content = .unavailable(location)
    }

    public func beginNavigation(to location: Location) -> NavigationRequest? { begin(location, .push) }
    public var hasPendingNavigation: Bool { pending != nil }
    /// Overlays do not change whether the retained directory is available.
    public var hasAvailableDirectory: Bool {
        if case .closed = status { return false }
        return directory != nil
    }

    public func beginRetry() -> NavigationRequest? {
        guard let (location, commit) = retry else { return nil }
        return begin(location, commit)
    }

    public func beginBack() -> NavigationRequest? { history.backTarget.flatMap { begin($0, .back) } }
    public func beginForward() -> NavigationRequest? { history.forwardTarget.flatMap { begin($0, .forward) } }

    public func beginRefresh() -> NavigationRequest? {
        if let pending { return begin(pending.request.location, pending.commit) }
        return begin(location, history.current != nil ? .replace : .push)
    }

    private func begin(_ location: Location, _ commit: HistoryCommit) -> NavigationRequest? {
        if case .closed = status { return nil }
        cancelPending()
        pageKeys.removeAll()
        navigationGeneration += 1
        let request = NavigationRequest(generation: navigationGeneration,
                                        request: DirectoryRequest(location: location, page: PageRequest(offset: 0, limit: Int.max)),
                                        commit: commit)
        status = .loading(location)
        retry = nil
        pending = request
        return request
    }

    @discardableResult
    public func applyDirectoryResult(generation: UInt64, _ result: RequestCompletion<DirectorySnapshot>) -> Bool {
        guard let pending, generation == navigationGeneration, result.id == pending.request.id else { return false }
        self.pending = nil
        var outcome = result.outcome
        if case .succeeded(let snapshot) = outcome,
           snapshot.location.requested != pending.request.location || snapshot.nextPage != nil {
            outcome = .failed(OperationError(.invalidRequest, source: pending.request.location))
        }
        switch outcome {
        case .succeeded(let snapshot):
            let resolved = snapshot.location.resolved
            switch pending.commit {
            case .push: history.push(resolved)
            case .back: history.back()
            case .forward: history.forward()
            case .replace: break
            }
            history.replaceCurrent(resolved)
            if location != resolved { selectionKeys.removeAll() }
            let present = Set(snapshot.entries.map { $0.location.uri })
            selectionKeys.removeAll { !present.contains($0) }
            content = .directory(resolved)
            location = resolved
            directory = snapshot
            status = .ready
        case .failed(let error):
            retry = (pending.request.location, pending.commit)
            status = .failed(location: pending.request.location, error: error)
        case .cancelled:
            status = directory != nil ? .ready : .idle
        }
        return true
    }

    /// Streaming pages use the same generation and request id. The first page commits history;
    /// later pages only extend that committed directory.
    @discardableResult
    public func applyDirectoryPage(generation: UInt64, _ incoming: RequestCompletion<DirectorySnapshot>) -> Bool {
        guard var pending, generation == navigationGeneration, incoming.id == pending.request.id else { return false }
        var result = incoming
        if case .succeeded(let snapshot) = result.outcome {
            let badPage: Bool = {
                if snapshot.location.requested != pending.request.location { return true }
                if let next = snapshot.nextPage,
                   next.limit == 0 || next.offset != pending.request.page.offset + snapshot.entries.count || next.offset <= pending.request.page.offset {
                    return true
                }
                if pending.request.page.offset > 0, let previous = directory, previous.location != snapshot.location { return true }
                return false
            }()
            if badPage {
                return applyDirectoryResult(generation: generation, RequestCompletion(
                    id: result.id, outcome: .failed(OperationError(.invalidRequest, source: pending.request.location))))
            }
        }
        var next: PageRequest?
        if case .succeeded(var snapshot) = result.outcome, snapshot.location.requested == pending.request.location {
            next = snapshot.nextPage
            snapshot.nextPage = nil
            result.outcome = .succeeded(snapshot)
        }
        if pending.request.page.offset > 0, case .succeeded(let snapshot) = result.outcome, var previous = directory {
            for entry in snapshot.entries where pageKeys.insert(entry.location).inserted { previous.entries.append(entry) }
            previous.nextPage = next
            previous.capabilities = snapshot.capabilities
            directory = previous
            if let next {
                pending.request.page = next
                self.pending = pending
            } else {
                self.pending = nil
                let present = Set(previous.entries.map { $0.location.uri })
                selectionKeys.removeAll { !present.contains($0) }
            }
            return true
        }
        let selected = selectionKeys
        var sameLocation = false
        if case .succeeded(var snapshot) = result.outcome {
            sameLocation = snapshot.location.resolved == location
            snapshot.entries = snapshot.entries.filter { pageKeys.insert($0.location).inserted }
            result.outcome = .succeeded(snapshot)
        }
        let accepted = applyDirectoryResult(generation: generation, result)
        if accepted, case .ready = status, let next {
            if next.offset <= pending.request.page.offset { return true }
            pending.request.page = next
            pending.commit = .replace
            self.pending = pending
            if sameLocation { selectionKeys = selected }
            directory?.nextPage = next
        }
        return accepted
    }

    public func cancelPending() {
        if let pending {
            self.pending = nil
            pending.request.cancellation.cancel()
        }
    }

    public func close() {
        cancelPending()
        retry = nil
        status = .closed
    }

    /// A removed mount invalidates its listing, never its tab identity or history.
    @discardableResult
    public func markUnavailable(root: Location) -> Bool {
        if case .closed = status { return false }
        let currentAffected = location.isWithin(root)
        let pendingAffected = pending.flatMap { $0.request.location.isWithin(root) ? $0 : nil }
        if currentAffected {
            directory = nil
            selectionKeys.removeAll()
            content = .unavailable(location)
        }
        var retryTarget: (Location, HistoryCommit)?
        if let pendingAffected {
            cancelPending()
            retryTarget = (pendingAffected.request.location, pendingAffected.commit)
        } else if currentAffected && pending == nil {
            retryTarget = (location, history.current != nil ? .replace : .push)
        }
        if let (target, commit) = retryTarget {
            retry = (target, commit)
            status = .failed(location: target, error: OperationError(.mountUnavailable, source: target))
        }
        return currentAffected || pendingAffected != nil
    }

    public func showSection(_ section: SidebarSection) {
        cancelPending()
        status = directory != nil ? .ready : .idle
        content = .sectionOverview(section: section, underlying: location)
    }

    public func restoreDirectory() {
        content = directory != nil ? .directory(location) : .unavailable(location)
    }
}

/// Main-thread navigation ownership. The repository owns all filesystem work.
public final class NavigationCoordinator {
    public let state: TabState
    private let repository: DirectoryRepository

    public init(state: TabState, repository: DirectoryRepository) {
        self.state = state
        self.repository = repository
    }

    @discardableResult public func navigate(to location: Location, changed: @escaping (Bool) -> Void = { _ in }) -> Bool {
        dispatch(state.beginNavigation(to: location), changed)
    }
    @discardableResult public func refresh(changed: @escaping (Bool) -> Void = { _ in }) -> Bool { dispatch(state.beginRefresh(), changed) }
    @discardableResult public func back(changed: @escaping (Bool) -> Void = { _ in }) -> Bool { dispatch(state.beginBack(), changed) }
    @discardableResult public func forward(changed: @escaping (Bool) -> Void = { _ in }) -> Bool { dispatch(state.beginForward(), changed) }
    @discardableResult public func retry(changed: @escaping (Bool) -> Void = { _ in }) -> Bool { dispatch(state.beginRetry(), changed) }
    public func closeTab() { state.close() }

    public func watch(_ location: Location, changed: @escaping () -> Void) -> WatchHandle? {
        repository.watch(location, changed: changed)
    }

    public func inspectVisibleMetadata(_ request: VisibleMetadataRequest, done: @escaping VisibleMetadataCallback) {
        repository.inspectVisibleMetadata(request, done: done)
    }

    private func dispatch(_ request: NavigationRequest?, _ changed: @escaping (Bool) -> Void) -> Bool {
        guard let request else { return false }
        repository.listPages(request.request) { [weak state] result in
            let accepted = state?.applyDirectoryPage(generation: request.generation, result) ?? false
            changed(accepted)
        }
        return true
    }
}
