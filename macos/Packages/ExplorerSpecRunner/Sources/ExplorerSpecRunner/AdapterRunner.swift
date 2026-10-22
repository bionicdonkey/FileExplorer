import Foundation
import ExplorerAdapters
import ExplorerDomain

/// The recorded run of one `operate` step.
struct OperationRun {
    var events: [OperationEvent]
    var terminal: OperationEvent {
        get throws {
            let terminals = events.filter(\.isTerminal).count
            guard terminals == 1 else { throw RunError("expected exactly one terminal event, saw \(terminals)") }
            return try require(events.last.flatMap { $0.isTerminal ? $0 : nil }, "the terminal event is not last")
        }
    }
}

/// What a step's `as` name stands for in an adapter scenario.
enum AdapterBound {
    case run(OperationRun)
    case inspect(RequestOutcome<CapabilitySnapshot>)
    /// Every completion a listing delivered, in order.
    case pages([RequestOutcome<DirectorySnapshot>])
    case visible(RequestOutcome<[VisibleMetadata]>)
    case search(SearchOutcome)
    /// An operation manager that persisted its history to a file of its own.
    case history(HistoryRun)
    /// Extraction: success, or the reason it gave.
    case extract(Result<Void, RunError>)
    /// Compression: the archive's path relative to the sandbox, or the reason it gave.
    case compress(Result<String, RunError>)
}

final class HistoryRun {
    let file: URL
    let directory: URL
    var ids: [UInt64] = []
    var states: [String] = []
    init(directory: URL) {
        self.directory = directory
        file = directory.appendingPathComponent("history.json")
    }
    deinit { try? FileManager.default.removeItem(at: directory) }
}

/// `kind: "adapter"` scenarios: the real adapters over a real temporary filesystem. Asynchronous
/// actions run to completion inside their own step, and the runner asserts the port rule that no
/// callback ran before the call returned.
final class AdapterWorld {
    let sandbox: Sandbox
    let specRoot: URL
    var bound: [String: AdapterBound] = [:]
    var nextOperationID: UInt64 = 1
    let operations: FileOperations = ArchiveFileOperations(wrapping: LocalFileOperations())
    let directory: DirectoryRepository = DirectoryRouter()
    let searcher = LocalSearchRepository()

    init(specRoot: URL) throws {
        self.specRoot = specRoot
        sandbox = try Sandbox(specRoot: specRoot)
    }

    /// A sandbox-relative path, or a URI when the text names a scheme.
    func locate(_ text: String) throws -> Location {
        // `in-archive:ARCHIVE!/INNER` names an entry inside a local archive; INNER is URI-escaped.
        if text.hasPrefix("in-archive:") {
            let rest = text.dropFirst("in-archive:".count)
            guard let split = rest.range(of: "!/") else { throw RunError("in-archive: needs `ARCHIVE!/INNER`") }
            let archive = String(rest[..<split.lowerBound]), inner = String(rest[split.upperBound...])
            let root = try Location(localPath: sandbox.path(archive)).archiveContents()
            return try location(root.uri + inner)
        }
        if text.contains("://") { return try location(text) }
        return Location(localPath: sandbox.path(text))
    }

    /// Runs the main run loop until `done` holds; fails if it does not within five seconds.
    func pump(_ done: () -> Bool) throws {
        let deadline = Date().addingTimeInterval(5)
        while !done() {
            RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.005))
            if Date() > deadline { throw RunError("no terminal result within five seconds") }
        }
    }
}

enum AdapterRunner {
    static func run(_ scenario: Scenario, mode: RunMode, specRoot: URL) throws {
        let world = try AdapterWorld(specRoot: specRoot)
        if (scenario.given.array("requires") as? [String])?.contains("unprivileged") == true, world.sandbox.isPrivileged {
            // A process that can read mode-000 files cannot exercise permission failures.
            return
        }
        try world.sandbox.build(scenario.given["tree"])
        for (index, step) in try scenario.effectiveSteps(platform: "macos").enumerated() {
            do { try perform(step, in: world, mode: mode) } catch let error as RunError { throw RunError("step \(index): \(error)") }
        }
    }

    static func perform(_ step: Step, in world: AdapterWorld, mode: RunMode) throws {
        let fields = step.fields
        switch step.form {
        case .action(let action):
            switch action {
            case "operate":
                let run = try operate(world, fields)
                world.nextOperationID += 1
                if mode == .excluded { return try expectUnsupported(run) }
                world.bound[fields.string("as") ?? "_"] = .run(run)
            case "inspect":
                let outcome = try inspect(world, fields)
                world.bound[fields.string("as") ?? "_"] = .inspect(outcome)
            case "list", "list_pages":
                let pages = try list(world, fields, paged: action == "list_pages")
                if mode == .excluded {
                    guard case .failed(let error)? = pages.last, error.kind == .unsupported else { throw RunError("excluded feature did not report unsupported") }
                    return
                }
                world.bound[fields.string("as") ?? "_"] = .pages(pages)
            case "search":
                let outcome = try search(world, fields)
                if mode == .excluded {
                    guard case .unsupported = outcome else { throw RunError("excluded feature did not report unsupported") }
                    return
                }
                world.bound[fields.string("as") ?? "_"] = .search(outcome)
            case "extract":
                world.bound[fields.string("as") ?? "_"] = .extract(try extract(world, fields))
            case "compress":
                world.bound[fields.string("as") ?? "_"] = .compress(try compress(world, fields))
            case "manage":
                world.bound[fields.string("as") ?? "_"] = .history(try manage(world, fields))
            case "inspect_visible":
                world.bound[fields.string("as") ?? "_"] = .visible(try inspectVisible(world, fields))
            case "mutate":
                try world.sandbox.build(fields["tree"])
                if let path = fields.string("remove") { try world.sandbox.remove(path) }
            default: throw RunError("action \(action) is not implemented by the adapter runner yet")
            }
        case .expectation(let name): try expectation(world, name, fields)
        case .portScript: throw RunError("adapter scenarios have no port scripts")
        }
    }

    // MARK: operate

    static func operate(_ world: AdapterWorld, _ step: JSON) throws -> OperationRun {
        let kind = try OperationsSubject.mutationKind(try require(step.string("kind"), "operate needs `kind`"))
        let request = OperationRequest(
            id: OperationID(world.nextOperationID), kind: kind,
            sources: try step.strings("sources").map(world.locate),
            destination: try step.string("destination").map(world.locate),
            explicitTarget: try step.string("target").map(world.locate),
            capabilityVersion: step.uint64("capability_version") ?? 1)
        var decision: ConflictDecision?
        if let name = step.string("decision") { decision = try OperationsSubject.decision(name) }
        var events: [OperationEvent] = []
        let sink: OperationSink = { events.append($0) }
        if let decision { world.operations.submit(request, decision: decision, sink: sink) } else { world.operations.submit(request, sink: sink) }
        if !events.isEmpty { throw RunError("an event ran inline before the adapter returned") }
        try world.pump { events.last?.isTerminal == true }
        // The runner checks this itself; scenarios do not repeat it.
        let sequences = events.map(\.sequence)
        if zip(sequences, sequences.dropFirst()).contains(where: { $0 >= $1 }) {
            throw RunError("event sequence numbers are not strictly increasing: \(sequences)")
        }
        let run = OperationRun(events: events)
        _ = try run.terminal
        return run
    }

    static func expectUnsupported(_ run: OperationRun) throws {
        guard case .failed(_, _, let error, _) = try run.terminal, error.kind == .unsupported else {
            throw RunError("excluded feature did not report unsupported")
        }
    }

    static func inspect(_ world: AdapterWorld, _ step: JSON) throws -> RequestOutcome<CapabilitySnapshot> {
        var received: RequestOutcome<CapabilitySnapshot>?
        let request = CapabilityRequest(location: try world.locate(try require(step.string("path"), "inspect needs `path`")))
        world.operations.inspectCapabilities(request) { received = $0.outcome }
        if received != nil { throw RunError("a callback ran inline before the adapter returned") }
        try world.pump { received != nil }
        return received!
    }

    // MARK: listings

    static func list(_ world: AdapterWorld, _ step: JSON, paged: Bool) throws -> [RequestOutcome<DirectorySnapshot>] {
        let cancellation = CancellationToken()
        let cancel = step.string("cancel")
        if cancel == "immediately" { cancellation.cancel() }
        let request = DirectoryRequest(
            location: try world.locate(try require(step.string("path"), "list needs `path`")),
            page: PageRequest(offset: step.int("offset") ?? 0, limit: step.int("limit") ?? Int.max), cancellation: cancellation)
        var pages: [RequestOutcome<DirectorySnapshot>] = []
        let record: DirectoryCallback = { completion in
            pages.append(completion.outcome)
            if cancel == "after_first_page" { cancellation.cancel() }
        }
        if paged { world.directory.listPages(request, sink: record) } else { world.directory.list(request, done: record) }
        if !pages.isEmpty { throw RunError("a callback ran inline before the adapter returned") }
        func finished() -> Bool {
            switch pages.last {
            case .succeeded(let snapshot)?: return snapshot.nextPage == nil
            case nil: return false
            default: return true
            }
        }
        try world.pump(finished)
        return pages
    }

    static func search(_ world: AdapterWorld, _ step: JSON) throws -> SearchOutcome {
        var scope = SearchScope.localFallback(root: try world.locate(step.string("root") ?? "."))
        if let depth = step.int("max_depth") { scope.maxDepth = UInt32(depth) }
        if let results = step.int("max_results") { scope.maxResults = results }
        if let ms = step.int("timeout_ms") { scope.timeout = TimeInterval(ms) / 1000 }
        let cancellation = CancellationToken()
        if step.string("cancel") == "immediately" { cancellation.cancel() }
        let mode: SearchMode
        switch step.string("mode") ?? "filename" {
        case "filename": mode = .filename
        case "content": mode = .content
        case let other: throw RunError("unknown search mode \(other)")
        }
        var received: SearchOutcome?
        world.searcher.search(SearchRequest(scope: scope, query: try require(step.string("query"), "search needs `query`"), mode: mode, cancellation: cancellation)) { received = $0.outcome }
        if received != nil { throw RunError("a callback ran inline before the adapter returned") }
        try world.pump { received != nil }
        return received!
    }

    /// Runs operations through the operation manager with history saved to a file outside the
    /// sandbox (so it never shows up in a tree check), and waits for each to finish.
    static func manage(_ world: AdapterWorld, _ step: JSON) throws -> HistoryRun {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("spec-history-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let run = HistoryRun(directory: directory)
        let manager = OperationManager(backend: world.operations, capabilities: FileExecutor.localCapabilities, historyFile: run.file, retentionDays: 7)
        for case let operation as JSON in try require(step.array("operations"), "manage needs `operations`") {
            let kind = try OperationsSubject.mutationKind(try require(operation.string("kind"), "operation needs `kind`"))
            let id = manager.submit(kind, sources: try operation.strings("sources").map(world.locate), destination: try operation.string("destination").map(world.locate))
            try world.pump { manager.records.first { $0.id == id }?.finishedAt != nil }
            let state: String
            switch manager.records.first(where: { $0.id == id })!.state {
            case .completed: state = "completed"
            case .failed: state = "failed"
            case .cancelled: state = "cancelled"
            default: state = "active"
            }
            run.ids.append(id)
            run.states.append(state)
        }
        return run
    }

    static func expectHistory(_ world: AdapterWorld, _ run: HistoryRun, _ step: JSON) throws {
        if step.has("states") { try expectEqual("operation states", run.states, step.strings("states")) }
        let forbidden = step.strings("persisted_lacks").map { $0 == "$sandbox" ? world.sandbox.root : $0 }
        let raw = try String(contentsOf: run.file, encoding: .utf8)
        for text in forbidden where raw.contains(text) { throw RunError("the saved history reveals \(text.debugDescription)") }
        if step.has("reloaded_records") || !forbidden.isEmpty {
            let reloaded = OperationManager(backend: world.operations, capabilities: FileExecutor.localCapabilities, historyFile: run.file, retentionDays: 7)
            if let count = step.int("reloaded_records") { try expectEqual("reloaded records", reloaded.records.count, count) }
            let shown = "\(reloaded.records) \(run.ids.map { reloaded.summary($0) as Any })"
            for text in forbidden where shown.contains(text) { throw RunError("the reloaded history reveals \(text.debugDescription)") }
        }
    }

    /// Extracts through the application's own entry point, as a job on an operation manager, and
    /// waits for it to finish.
    static func extract(_ world: AdapterWorld, _ step: JSON) throws -> Result<Void, RunError> {
        let manager = OperationManager(backend: world.operations, capabilities: FileExecutor.localCapabilities)
        let archive = try world.locate(try require(step.string("archive"), "extract needs `archive`"))
        let parent = try world.locate(step.string("into") ?? ".")
        var failure: String?
        guard ArchiveTools.extract(manager: manager, archive: archive, parent: parent, onFailure: { failure = $0 }) else {
            return .failure(RunError("extraction did not begin"))
        }
        try world.pump { manager.records.allSatisfy { !$0.state.isActive } && !manager.records.isEmpty }
        // The failure message is delivered just after the job finishes.
        try world.pump { failure != nil || manager.records.last?.state == .completed }
        if let failure { return .failure(RunError(failure)) }
        return manager.records.last?.state == .completed ? .success(()) : .failure(RunError("extraction ended as \(String(describing: manager.records.last?.state))"))
    }

    static func compress(_ world: AdapterWorld, _ step: JSON) throws -> Result<String, RunError> {
        let format: ArchiveFormat
        switch try require(step.string("format"), "compress needs `format`") {
        case "zip": format = .zip
        case "7z": format = .sevenZip
        case "tar_gz": format = .tarGz
        case let other: throw RunError("unknown archive format \(other)")
        }
        var progress: [(UInt64, UInt64)] = []
        do {
            let path = try ArchiveTools.compressBlocking(sources: step.strings("sources").map(world.sandbox.path), format: format,
                                                         progress: { progress.append(($0, $1)) }, cancelled: { false })
            // Progress climbs to the size of the data and ends there.
            if let first = progress.first, let last = progress.last, first.0 != 0 || last.0 != last.1 { throw RunError("progress ran \(first) to \(last)") }
            return .success(String(path.dropFirst(world.sandbox.root.count + 1)))
        } catch let error as ArchiveError { return .failure(RunError(error.message)) }
    }

    static func inspectVisible(_ world: AdapterWorld, _ step: JSON) throws -> RequestOutcome<[VisibleMetadata]> {
        let cancellation = CancellationToken()
        if step.string("cancel") == "immediately" { cancellation.cancel() }
        let request = VisibleMetadataRequest(locations: try step.strings("paths").map(world.locate), cancellation: cancellation)
        var received: RequestOutcome<[VisibleMetadata]>?
        world.directory.inspectVisibleMetadata(request) { received = $0.outcome }
        if received != nil { throw RunError("a callback ran inline before the adapter returned") }
        try world.pump { received != nil }
        return received!
    }

    // MARK: expectations

    static func outcomeLabel<T>(_ outcome: RequestOutcome<T>) -> String {
        switch outcome {
        case .succeeded: return "succeeded"
        case .failed: return "failed"
        case .cancelled: return "cancelled"
        }
    }

    static func checkFailure<T>(_ outcome: RequestOutcome<T>, _ expected: JSON?) throws {
        if case .failed(let error) = outcome, let kind = expected?.string("kind") { try expectEqual("error kind", errorKindName(error.kind), kind) }
    }

    static func matchesPattern(_ names: [String], _ pattern: ArraySlice<Any>) -> Bool {
        guard let head = pattern.first else { return names.isEmpty }
        let rest = pattern.dropFirst()
        if let map = head as? JSON, let name = map["any"] as? String {
            var taken = 0
            while true {
                if matchesPattern(Array(names[taken...]), rest) { return true }
                if taken < names.count, names[taken] == name { taken += 1 } else { return false }
            }
        }
        if let map = head as? JSON, let name = map["some"] as? String {
            var taken = 0
            while taken < names.count, names[taken] == name {
                taken += 1
                if matchesPattern(Array(names[taken...]), rest) { return true }
            }
            return false
        }
        guard let literal = head as? String, let first = names.first else { return false }
        return first == literal && matchesPattern(Array(names.dropFirst()), rest)
    }

    static func relative(_ locations: [Location], _ world: AdapterWorld) -> [String] {
        locations.map { location in
            guard let path = location.localPath, path.hasPrefix(world.sandbox.root + "/") else { return location.uri }
            return String(path.dropFirst(world.sandbox.root.count + 1))
        }
    }

    static func expectation(_ world: AdapterWorld, _ name: String, _ step: JSON) throws {
        func bound() throws -> AdapterBound {
            let ref = try require(step.string("ref"), "expectation needs `ref`").trimmingCharacters(in: CharacterSet(charactersIn: "$"))
            return try require(world.bound[ref], "no step bound \(ref)")
        }
        func run() throws -> OperationRun {
            guard case .run(let run) = try bound() else { throw RunError("that step did not run an operation") }
            return run
        }
        switch name {
        case "outcome":
            let want = try require(step.string("is"), "outcome needs `is`")
            switch try bound() {
            case .run(let run):
                let terminal = try run.terminal
                try expectEqual("outcome", terminal.name, want)
                if case .failed(_, _, let error, _) = terminal, let expected = step.dict("error") {
                    if let kind = expected.string("kind") { try expectEqual("error kind", errorKindName(error.kind), kind) }
                    if let retry = expected.string("retry") {
                        let got: String
                        switch error.retry {
                        case .never: got = "never"
                        case .afterRefresh: got = "after_refresh"
                        case .afterUserAction: got = "after_user_action"
                        case .transient: got = "transient"
                        }
                        try expectEqual("retry class", got, retry)
                    }
                }
            case .inspect(let outcome):
                try expectEqual("outcome", outcomeLabel(outcome), want)
                try checkFailure(outcome, step.dict("error"))
            case .pages(let pages):
                let last = try require(pages.last, "the listing delivered nothing")
                try expectEqual("outcome", outcomeLabel(last), want)
                try checkFailure(last, step.dict("error"))
            case .visible(let outcome):
                try expectEqual("outcome", outcomeLabel(outcome), want)
            case .extract(let result):
                let got = { if case .success = result { return "succeeded" } else { return "failed" } }()
                if got != want { throw RunError("extraction \(got), expected \(want): \(result)") }
            case .compress(let result):
                let got = { if case .success = result { return "succeeded" } else { return "failed" } }()
                if got != want { throw RunError("compression \(got), expected \(want): \(result)") }
                if case .success(let path) = result, let expected = step.string("archive"), path != expected { throw RunError("archive is \(path.debugDescription), expected \(expected.debugDescription)") }
            case .search, .history: throw RunError("this step has no single outcome")
            }
        case "effect":
            let completion = try require(try run().terminal.completion, "no completion")
            try expectEqual("effect", completion.effect.rawValue, try require(step.string("is"), "effect needs `is`"))
            if step.has("completed") { try expectEqual("completed", relative(completion.completed, world), step.strings("completed")) }
            if step.has("remaining") { try expectEqual("remaining", relative(completion.remaining, world), step.strings("remaining")) }
        case "events":
            let names = try run().events.map(\.name)
            if !matchesPattern(names, ArraySlice(try require(step.array("match"), "events needs `match`"))) {
                throw RunError("events \(names) do not match \(step["match"] ?? "nil")")
            }
        case "tree":
            for path in step.strings("contains") where PathInfo.at(world.sandbox.path(path)) == .missing { throw RunError("expected \(path) to exist") }
            for path in step.strings("absent") where PathInfo.at(world.sandbox.path(path)) != .missing { throw RunError("expected \(path) to be absent") }
            for (path, want) in step.dict("contents") ?? [:] {
                let got = try String(contentsOfFile: world.sandbox.path(path), encoding: .utf8)
                if got != (want as? String) { throw RunError("\(path) holds \(got.debugDescription), expected \(want)") }
            }
        case "entries":
            guard case .pages(let pages) = try bound() else { throw RunError("that step did not list a directory") }
            var names = pages.flatMap { outcome -> [String] in
                if case .succeeded(let s) = outcome { return s.entries.map(\.displayName) } else { return [] }
            }
            var want = step.strings("names")
            if step.string("order") != "exact" { names.sort(); want.sort() }
            try expectEqual("entries", names, want)
        case "pages":
            guard case .pages(let pages) = try bound() else { throw RunError("that step did not list a directory") }
            let counts = pages.compactMap { outcome -> Int? in if case .succeeded(let s) = outcome { return s.entries.count } else { return nil } }
            if let max = step.int("first_page_max") {
                let first = try require(counts.first, "no page delivered")
                if first > max { throw RunError("the first page holds \(first) entries, more than \(max)") }
            }
            if let total = step.int("total") { try expectEqual("entries across pages", counts.reduce(0, +), total) }
            if step.string("terminal") == "exactly_one" {
                func continues(_ o: RequestOutcome<DirectorySnapshot>) -> Bool { if case .succeeded(let s) = o { return s.nextPage != nil } else { return false } }
                let terminals = pages.filter { !continues($0) }.count
                if terminals != 1 || continues(pages.last!) { throw RunError("expected exactly one terminal page, last, saw \(terminals)") }
            }
        case "entry":
            guard case .pages(let pages) = try bound() else { throw RunError("that step did not list a directory") }
            let snapshot = try require(pages.compactMap { outcome -> DirectorySnapshot? in if case .succeeded(let s) = outcome { return s } else { return nil } }.first, "the listing did not succeed")
            let name = try require(step.string("name"), "entry needs `name`")
            let entry = try require(snapshot.entries.first { $0.displayName == name }, "no entry named \(name)")
            if let kind = step.string("kind") { try expectEqual("\(name) kind", entry.kind.rawValue, kind) }
            for (field, present) in [("size", entry.size != nil), ("modified", entry.modified != nil), ("content_type", entry.contentType != nil)] where step.has(field) {
                if step.isNull(field) { if present { throw RunError("\(name) \(field) was loaded eagerly") } }
                else { throw RunError("entry \(field) can only be checked for null, not \(step[field] ?? "nil")") }
            }
            for cap in step.strings("has") where !entry.capabilities.supports(try OperationsSubject.capability(cap)) { throw RunError("\(name) lacks \(cap)") }
            for cap in step.strings("lacks") where entry.capabilities.supports(try OperationsSubject.capability(cap)) { throw RunError("\(name) has \(cap)") }
            if let path = step.string("path") { try expectEqual("\(name) location", entry.location, try world.locate(path)) }
        case "metadata":
            guard case .visible(.succeeded(let rows)) = try bound() else { throw RunError("no visible metadata was delivered") }
            let path = try require(step.string("path"), "metadata needs `path`")
            let wanted = try world.locate(path)
            let row = try require(rows.first { $0.location == wanted }, "no metadata for \(path)")
            if let size = step.uint64("size") { try expectEqual("\(path) size", row.size, size) }
            for (field, present) in [("has_modified", row.modified != nil), ("has_content_type", row.contentType != nil)] {
                if let want = step.bool(field) { try expectEqual("\(path) \(field)", present, want) }
            }
        case "history":
            guard case .history(let run) = try bound() else { throw RunError("that step did not run the operation manager") }
            try expectHistory(world, run, step)
        case "search":
            guard case .search(let outcome) = try bound() else { throw RunError("that step did not run a search") }
            var label = "unsupported", matches: [EntrySnapshot]?, limits: [SearchLimit] = [], scopeLimited: Bool?, reason: SearchIncompleteReason?
            switch outcome {
            case .completed(let m, _, let limited, let l): label = "completed"; matches = m; limits = l; scopeLimited = limited
            case .incomplete(let m, let r, _, _, _): label = "incomplete"; matches = m; reason = r
            case .unsupported: break
            }
            try expectEqual("search", label, try require(step.string("is"), "search needs `is`"))
            if step.has("names") { try expectEqual("matches", (matches ?? []).map(\.displayName), step.strings("names")) }
            // `limits` names bounds that were in force, so the expectation is inclusion.
            for want in step.strings("limits") where !limits.contains(where: { $0.rawValue == want }) { throw RunError("limits \(limits) do not include \(want)") }
            if step.has("paths") { try expectEqual("match locations", (matches ?? []).map(\.location), try step.strings("paths").map(world.locate)) }
            if let want = step.bool("scope_limited") { try expectEqual("scope_limited", scopeLimited, want) }
            if let want = step.string("reason") { try expectEqual("reason", reason?.rawValue, want) }
        case "capability":
            var capsOrNil: CapabilitySnapshot?
            switch try bound() {
            case .inspect(.succeeded(let caps)): capsOrNil = caps
            case .pages(let pages): capsOrNil = pages.compactMap { outcome -> CapabilitySnapshot? in if case .succeeded(let s) = outcome { return s.capabilities } else { return nil } }.first
            default: break
            }
            guard let caps = capsOrNil else { throw RunError("no capability snapshot to inspect") }
            for cap in step.strings("has") where !caps.supports(try OperationsSubject.capability(cap)) { throw RunError("capabilities lack \(cap)") }
            for cap in step.strings("lacks") where caps.supports(try OperationsSubject.capability(cap)) { throw RunError("capabilities have \(cap)") }
            if let want = step.bool("stable_order") { try expectEqual("stable_order", caps.stableOrder, want) }
        default: throw RunError("expectation \(name) is not implemented by the adapter runner yet")
        }
    }
}
