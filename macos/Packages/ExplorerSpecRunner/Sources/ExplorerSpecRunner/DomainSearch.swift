import Foundation
import ExplorerDomain

final class HeldSearch {
    let request: SearchRequest
    var done: SearchCallback?
    init(request: SearchRequest, done: @escaping SearchCallback) {
        self.request = request
        self.done = done
    }
}

final class ScriptedSearch: SearchRepository {
    var held: [HeldSearch] = []
    func search(_ request: SearchRequest, done: @escaping SearchCallback) { held.append(HeldSearch(request: request, done: done)) }
}

/// Virtual time for the debounce. Nothing sleeps: `advance` runs the work that falls due.
final class ManualClock: Scheduler {
    private var nowMs = 0.0
    private var waiters: [(due: Double, work: () -> Void)] = []

    func schedule(after seconds: TimeInterval, _ work: @escaping () -> Void) {
        waiters.append((nowMs + seconds * 1000, work))
    }

    func advance(ms: Double) {
        nowMs += ms
        let due = waiters.filter { $0.due <= nowMs }
        waiters.removeAll { $0.due <= nowMs }
        due.forEach { $0.work() }
    }
}

/// The `search`, `clock` and `search_history` subjects. The coordinator shares the navigation tab.
final class SearchSubject {
    let repository = ScriptedSearch()
    let clock = ManualClock()
    let coordinator: SearchCoordinator
    private var historyDirectory: URL?
    private var history: SearchHistory?

    init(tab: TabState) {
        coordinator = SearchCoordinator(tab: tab, repository: repository, scheduler: clock)
    }

    deinit { historyDirectory.map { try? FileManager.default.removeItem(at: $0) } }

    private func mode(_ step: JSON) throws -> SearchMode {
        switch step.string("mode") ?? "filename" {
        case "filename": return .filename
        case "content": return .content
        case let other: throw RunError("unknown search mode \(other)")
        }
    }

    /// `scope` overrides fields of the default (`localFallback` of the tab's location).
    private func scope(_ world: DomainWorld, _ over: JSON?) throws -> SearchScope {
        let over = over ?? [:]
        let root = try over.string("root").map(location) ?? world.tab.location
        var scope = SearchScope.localFallback(root: root)
        if let depth = over.int("max_depth") { scope.maxDepth = UInt32(depth) }
        if let results = over.int("max_results") { scope.maxResults = results }
        if let ms = over.int("timeout_ms") { scope.timeout = TimeInterval(ms) / 1000 }
        return scope
    }

    private func check(_ step: JSON, accepted: Bool) throws {
        if let want = step.bool("accepted"), want != accepted { throw RunError("action accepted is \(accepted), expected \(want)") }
    }

    func action(_ world: DomainWorld, _ action: String, _ step: JSON) throws {
        switch action {
        case "submit", "type":
            let text = try require(step.string("text"), "needs `text`")
            let scope = try scope(world, step.dict("scope"))
            let accepted = action == "type"
                ? coordinator.submitDebounced(text, mode: try mode(step), scope: scope)
                : coordinator.submit(text, mode: try mode(step), scope: scope)
            try check(step, accepted: accepted)
        case "retry": try check(step, accepted: coordinator.retry())
        case "expand_scope": try check(step, accepted: coordinator.expandScope(try scope(world, step.dict("scope"))))
        case "clear": try check(step, accepted: coordinator.clear())
        default: throw RunError("search has no action \(action)")
        }
    }

    func clockAction(_ action: String, _ step: JSON) throws {
        guard action == "advance" else { throw RunError("clock has no action \(action)") }
        clock.advance(ms: Double(try require(step.int("ms"), "advance needs `ms`")))
    }

    private func entries(_ uris: [String]) throws -> [EntrySnapshot] {
        try uris.map { uri in
            let loc = try location(uri)
            return EntrySnapshot(location: loc, displayName: loc.displayName, kind: .file)
        }
    }

    private func reason(_ name: String) throws -> SearchIncompleteReason {
        guard let value = SearchIncompleteReason(rawValue: name) else { throw RunError("unknown incomplete reason \(name)") }
        return value
    }

    func scriptPort(_ step: JSON) throws {
        let entry = repository.held[try requestIndex(step, count: repository.held.count)]
        let done = try require(entry.done, "that search was already answered")
        entry.done = nil
        let declared = entry.request.scope
        let respond = step.dict("respond") ?? [:]
        let outcome: SearchOutcome
        if let body = respond.dict("completed") {
            let limits = try body.strings("limits").map { name -> SearchLimit in
                guard let limit = SearchLimit(rawValue: name) else { throw RunError("unknown limit \(name)") }
                return limit
            }
            outcome = .completed(matches: try entries(body.strings("matches")), declaredScope: declared, scopeLimited: body.bool("scope_limited") ?? false, limits: limits)
        } else if let body = respond.dict("incomplete") {
            outcome = .incomplete(matches: try entries(body.strings("matches")), reason: try reason(try require(body.string("reason"), "incomplete needs `reason`")),
                                  declaredScope: declared, retry: .afterRefresh, error: nil)
        } else if respond.has("unsupported") {
            outcome = .unsupported(declaredScope: declared, error: OperationError(.unsupported))
        } else if let body = respond.dict("wrong_scope") {
            var other = declared
            other.maxResults = body.int("max_results") ?? 1
            outcome = .completed(matches: [], declaredScope: other, scopeLimited: false, limits: [])
        } else {
            throw RunError("unknown `respond`: \(step["respond"] ?? "nil")")
        }
        done(SearchCompletion(id: entry.request.id, outcome: outcome))
    }

    func expectPortRequests(_ step: JSON) throws {
        let held = repository.held
        if let count = step.int("count"), held.count != count { throw RunError("search port received \(held.count) request(s), expected \(count)") }
        guard let last = step.dict("last") else { return }
        let request = try require(held.last, "no search request received").request
        if let query = last.string("query") { try expectEqual("last request query", request.query, query) }
        if let root = last.string("root") { try expectEqual("last request root", request.scope.root.uri, root) }
        if let depth = last.int("max_depth") { try expectEqual("last request max_depth", Int(request.scope.maxDepth), depth) }
        if let results = last.int("max_results") { try expectEqual("last request max_results", request.scope.maxResults, results) }
        if let ms = last.int("timeout_ms") { try expectEqual("last request timeout_ms", Int((request.scope.timeout * 1000).rounded()), ms) }
    }

    func expectCancelled(_ step: JSON) throws {
        let index = try requestIndex(step, count: repository.held.count)
        let want = step.bool("is") ?? true
        let got = repository.held[index].request.cancellation.isCancelled
        if got != want { throw RunError("search request cancelled is \(got), expected \(want)") }
    }

    /// `status` is `none` until an accepted outcome arrives, then `completed`, `incomplete` or
    /// `unsupported`. `content` says what the tab shows; `matches` lists the retained results.
    func expectView(_ world: DomainWorld, _ step: JSON) throws {
        let tab = world.tab
        if let want = step.string("status") {
            let got: String
            switch coordinator.outcome {
            case nil: got = "none"
            case .completed?: got = "completed"
            case .incomplete?: got = "incomplete"
            case .unsupported?: got = "unsupported"
            }
            try expectEqual("status", got, want)
        }
        if let want = step.string("reason") {
            var got = "none"
            if case .incomplete(_, let reason, _, _, _)? = coordinator.outcome { got = reason.rawValue }
            try expectEqual("reason", got, want)
        }
        if let want = step.string("content") {
            let got: String
            switch tab.content {
            case .directory: got = "directory"
            case .sectionOverview: got = "section"
            case .search: got = "search"
            case .unavailable: got = "unavailable"
            }
            try expectEqual("content", got, want)
        }
        if step.has("matches") {
            var got: [String] = []
            if case .search(_, let items) = tab.content { got = items.map(\.location.uri) }
            try expectEqual("matches", got, step.strings("matches"))
        }
        if step.has("selection") { try expectEqual("selection", tab.selectionKeys, step.strings("selection")) }
        if let want = step.bool("can_go_back") { try expectEqual("can_go_back", tab.history.canGoBack, want) }
    }

    // MARK: history

    private func historyFile() throws -> URL {
        if historyDirectory == nil {
            let dir = FileManager.default.temporaryDirectory.appendingPathComponent("spec-history-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            historyDirectory = dir
        }
        return historyDirectory!.appendingPathComponent("searches.json")
    }

    func historyAction(_ action: String, _ step: JSON) throws {
        let file = try historyFile()
        if history == nil { history = SearchHistory.load(from: file) }
        switch action {
        case "record_submitted":
            try history!.recordSubmitted(try require(step.string("text"), "needs `text`"), enabled: try require(step.bool("enabled"), "needs `enabled`"),
                                         retentionDays: step.int("retention_days") ?? 30)
        default: throw RunError("search_history has no action \(action)")
        }
    }

    func expectHistory(_ step: JSON) throws {
        let file = try historyFile()
        if let want = step.int("entries") { try expectEqual("history entries", history?.entries.count ?? 0, want) }
        if let want = step.int("persisted_entries") { try expectEqual("saved history entries", SearchHistory.load(from: file).entries.count, want) }
    }
}
