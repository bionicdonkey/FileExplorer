import Foundation

/// Runs `work` after a delay. Production uses the main queue; scenarios use a hand-driven clock so
/// nothing sleeps.
public protocol Scheduler: AnyObject {
    func schedule(after seconds: TimeInterval, _ work: @escaping () -> Void)
}

public final class MainQueueScheduler: Scheduler {
    public init() {}
    public func schedule(after seconds: TimeInterval, _ work: @escaping () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }
}

/// Search owns a temporary overlay on a tab, never navigation history or persisted terms. Typing
/// is debounced: each keystroke supersedes the previous query at once, and only the last reaches
/// the provider. A search is refused while the tab has no loaded directory or is navigating.
public final class SearchCoordinator {
    public static let debounce: TimeInterval = 0.18

    private let tab: TabState
    private let repository: SearchRepository
    private let scheduler: Scheduler
    private var request: SearchRequest?
    private var pending = false
    private var generation: UInt64 = 0
    private var saved: (generation: UInt64, directory: DirectorySnapshot, selection: [String])?
    public private(set) var outcome: SearchOutcome?

    public init(tab: TabState, repository: SearchRepository, scheduler: Scheduler = MainQueueScheduler()) {
        self.tab = tab
        self.repository = repository
        self.scheduler = scheduler
    }

    private func prepare(_ query: String, _ mode: SearchMode, _ scope: SearchScope) -> SearchRequest? {
        guard tab.hasAvailableDirectory, !tab.hasPendingNavigation else { return nil }
        request?.cancellation.cancel()
        if saved == nil, let directory = tab.directory {
            saved = (tab.navigationGeneration, directory, tab.selectionKeys)
        }
        generation = tab.navigationGeneration
        let new = SearchRequest(scope: scope, query: query.trimmingCharacters(in: .whitespacesAndNewlines), mode: mode)
        request = new
        pending = true
        return new
    }

    @discardableResult
    public func submit(_ query: String, mode: SearchMode, scope: SearchScope, changed: @escaping (SearchOutcome) -> Void = { _ in }) -> Bool {
        guard let request = prepare(query, mode, scope) else { return false }
        dispatch(request, changed)
        return true
    }

    /// Invalidates the previous query immediately, including during the debounce.
    @discardableResult
    public func submitDebounced(_ query: String, mode: SearchMode, scope: SearchScope, changed: @escaping (SearchOutcome) -> Void = { _ in }) -> Bool {
        guard let request = prepare(query, mode, scope) else { return false }
        scheduler.schedule(after: SearchCoordinator.debounce) { [self] in
            if !request.cancellation.isCancelled { dispatch(request, changed) }
        }
        return true
    }

    private func dispatch(_ request: SearchRequest, _ changed: @escaping (SearchOutcome) -> Void) {
        repository.search(request) { [weak self] completion in
            guard let self, let current = self.request else { return }
            guard pending, current.id == completion.id, !current.cancellation.isCancelled,
                  generation == tab.navigationGeneration, tab.hasAvailableDirectory else { return }
            let declared: SearchScope
            switch completion.outcome {
            case .completed(_, let scope, _, _), .incomplete(_, _, let scope, _, _), .unsupported(let scope, _): declared = scope
            }
            var outcome = completion.outcome
            if declared != current.scope {
                // A provider that answers for a scope other than the one asked is not trusted.
                outcome = .incomplete(matches: [], reason: .malformedPage, declaredScope: current.scope, retry: .afterRefresh, error: nil)
            }
            var items: [FileItem] = []
            switch outcome {
            case .completed(let matches, let scope, let limited, let limits):
                let unique = SearchCoordinator.deduplicated(matches)
                outcome = .completed(matches: unique, declaredScope: scope, scopeLimited: limited, limits: limits)
                items = unique.map(FileItem.init(snapshot:))
            case .incomplete(let matches, let reason, let scope, let retry, let error):
                let unique = SearchCoordinator.deduplicated(matches)
                outcome = .incomplete(matches: unique, reason: reason, declaredScope: scope, retry: retry, error: error)
                items = unique.map(FileItem.init(snapshot:))
            case .unsupported: break
            }
            tab.content = .search(underlying: tab.location, items: items)
            tab.selectionKeys.removeAll()
            pending = false
            self.outcome = outcome
            changed(outcome)
        }
    }

    private static func deduplicated(_ matches: [EntrySnapshot]) -> [EntrySnapshot] {
        var seen = Set<String>()
        return matches.filter { seen.insert($0.location.uri).inserted }
    }

    @discardableResult
    public func retry(changed: @escaping (SearchOutcome) -> Void = { _ in }) -> Bool {
        guard let request else { return false }
        return submit(request.query, mode: request.mode, scope: request.scope, changed: changed)
    }

    @discardableResult
    public func expandScope(_ scope: SearchScope, changed: @escaping (SearchOutcome) -> Void = { _ in }) -> Bool {
        guard let request else { return false }
        return submit(request.query, mode: request.mode, scope: scope, changed: changed)
    }

    /// Leaves search: cancels the query and restores the directory and selection that were there.
    @discardableResult
    public func clear() -> Bool {
        let wasActive = request != nil
        request?.cancellation.cancel()
        request = nil
        pending = false
        outcome = nil
        if let saved {
            self.saved = nil
            if saved.generation == tab.navigationGeneration, tab.hasAvailableDirectory {
                tab.selectionKeys = saved.selection
                tab.restoreDirectory()
            }
        }
        return wasActive
    }
}

// MARK: History

public struct SearchHistoryEntry: Equatable, Codable {
    public var term: String
    public var recordedAt: Int64
    enum CodingKeys: String, CodingKey { case term, recordedAt = "recorded_at" }
}

/// Saved search terms, subject to a privacy preference: disabling recording removes every stored
/// term, in memory and on disk.
public final class SearchHistory {
    public static let maximumRecords = 100
    private struct Stored: Codable { var entries: [SearchHistoryEntry] = [] }

    private let path: URL?
    public private(set) var entries: [SearchHistoryEntry] = []

    public static func inMemory() -> SearchHistory { SearchHistory(path: nil) }
    private init(path: URL?) { self.path = path }

    public static func load(from path: URL) -> SearchHistory {
        let history = SearchHistory(path: path)
        if let data = try? Data(contentsOf: path), let stored = try? JSONDecoder().decode(Stored.self, from: data) {
            history.entries = Array(stored.entries.suffix(maximumRecords))
        }
        return history
    }

    public func record(_ term: String, now: Date = Date()) {
        let term = term.trimmingCharacters(in: .whitespacesAndNewlines)
        if term.isEmpty || term.utf8.count > 4096 { return }
        entries.removeAll { $0.term == term }
        entries.append(SearchHistoryEntry(term: term, recordedAt: Int64(now.timeIntervalSince1970)))
        entries = Array(entries.suffix(SearchHistory.maximumRecords))
    }

    public func prune(retentionDays: Int, now: Date = Date()) {
        let cutoff = Int64(now.timeIntervalSince1970) - Int64(retentionDays) * 86_400
        entries.removeAll { $0.recordedAt < cutoff }
    }

    public func clear() { entries.removeAll() }

    /// Shared by startup and preferences, so disabled recording also removes terms left by an
    /// earlier session in which it was on.
    public func enforcePolicy(enabled: Bool, retentionDays: Int) throws {
        if enabled { prune(retentionDays: retentionDays) } else { clear() }
        try save()
    }

    public func recordSubmitted(_ term: String, enabled: Bool, retentionDays: Int) throws {
        if enabled { record(term) }
        try enforcePolicy(enabled: enabled, retentionDays: retentionDays)
    }

    public func save() throws {
        guard let path else { return }
        try FileManager.default.createDirectory(at: path.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(Stored(entries: entries)).write(to: path, options: .atomic)
    }
}
