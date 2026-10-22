import Foundation
import ExplorerDomain

final class HeldInspection {
    let request: CapabilityRequest
    var done: CapabilityCallback?
    init(request: CapabilityRequest, done: @escaping CapabilityCallback) {
        self.request = request
        self.done = done
    }
}

final class HeldOperation {
    let request: OperationRequest
    let sink: OperationSink
    var nextSequence: UInt64 = 1
    init(request: OperationRequest, sink: @escaping OperationSink) {
        self.request = request
        self.sink = sink
    }
}

/// A backend that holds every capability inspection and every dispatched operation until a step
/// answers it. Event sequence numbers are assigned in order per dispatched operation.
final class ScriptedOperations: FileOperations {
    var inspections: [HeldInspection] = []
    var operations: [HeldOperation] = []

    func inspectCapabilities(_ request: CapabilityRequest, done: @escaping CapabilityCallback) {
        inspections.append(HeldInspection(request: request, done: done))
    }

    func submit(_ request: OperationRequest, sink: @escaping OperationSink) {
        operations.append(HeldOperation(request: request, sink: sink))
    }
}

/// The `operations` and `reducer` subjects.
final class OperationsSubject {
    let backend = ScriptedOperations()
    let coordinator: OperationCoordinator
    var reducer: OperationReducer?

    init() { coordinator = OperationCoordinator(backend: backend) }

    static func capability(_ name: String) throws -> Capability {
        switch name {
        case "list": return .list
        case "copy": return .copy
        case "move": return .move
        case "create": return .create
        case "rename": return .rename
        case "trash": return .trash
        case "delete": return .delete
        case "open": return .open
        case "search_filename": return .searchFilename
        case "search_content": return .searchContent
        default: throw RunError("unknown capability \(name)")
        }
    }

    /// The scenario default overlaid with the step's `capabilities`: `version`, `supported`
    /// (replaces), `with`, `without`, `containment` ("opaque" or `{"ancestors": [uri]}`), `stable_order`.
    static func snapshot(_ world: DomainWorld, _ over: JSON?) throws -> CapabilitySnapshot {
        let over = over ?? [:]
        func field(_ name: String) -> Any? { over[name] ?? world.defaultCapabilities[name] }
        var supported = try ((field("supported") as? [Any]) ?? []).compactMap { $0 as? String }.map(capability)
        supported += try over.strings("with").map(capability)
        for name in over.strings("without") { let gone = try capability(name); supported.removeAll { $0 == gone } }
        let containment: Containment
        switch field("containment") {
        case nil, is NSNull: containment = .opaque
        case let text as String where text == "opaque": containment = .opaque
        case let map as JSON: containment = .knownAncestors(try map.strings("ancestors").map(location))
        default: throw RunError("bad containment \(field("containment") ?? "nil")")
        }
        let version = (field("version") as? NSNumber)?.uint64Value ?? 1
        let stable = (field("stable_order") as? NSNumber)?.boolValue ?? true
        return CapabilitySnapshot(version: version, supported: Set(supported), stableOrder: stable, containment: containment)
    }

    static func mutationKind(_ name: String) throws -> MutationKind {
        guard let kind = MutationKind(rawValue: name) else { throw RunError("unknown operation kind \(name)") }
        return kind
    }

    static func request(_ step: JSON) throws -> OperationRequest {
        OperationRequest(
            id: OperationID(try require(step.uint64("id"), "needs a numeric `id`")),
            kind: try mutationKind(step.string("kind") ?? "copy"),
            sources: try step.strings("sources").map(location),
            destination: try step.string("destination").map(location),
            explicitTarget: try step.string("target").map(location),
            capabilityVersion: step.uint64("capability_version") ?? 1)
    }

    static func decision(_ name: String) throws -> ConflictDecision {
        switch name {
        case "replace": return .replace
        case "keep_both": return .keepBoth
        case "skip": return .skip
        case "cancel_remaining": return .cancelRemaining
        default: throw RunError("unknown decision \(name)")
        }
    }

    func resolveID(_ world: DomainWorld, _ step: JSON, _ field: String) throws -> OperationID {
        if let n = step.uint64(field) { return OperationID(n) }
        guard let name = step.string(field) else { throw RunError("bad `\(field)`: \(step[field] ?? "nil")") }
        switch world.bound[name.trimmingCharacters(in: CharacterSet(charactersIn: "$"))] {
        case .op(let id)?: return id
        case .submit(.success(let id))?: return id
        default: throw RunError("\(name) is not a bound operation")
        }
    }

    func bind(_ world: DomainWorld, _ step: JSON, op id: OperationID?, result: Bound?) {
        if let id, let name = step.string("as") { world.bound[name] = .op(id) }
        if let result, let name = step.string("as_result") { world.bound[name] = result }
    }

    // MARK: actions

    func action(_ world: DomainWorld, _ action: String, _ step: JSON) throws {
        switch action {
        case "submit":
            let request = try OperationsSubject.request(step)
            let id = request.id
            let caps = try OperationsSubject.snapshot(world, step.dict("capabilities"))
            let result: Result<OperationID, OperationError>
            do {
                if step.bool("confirmed_delete") == true { result = .success(try coordinator.submitConfirmedDelete(request, capabilities: caps)) }
                else if let name = step.string("decision") { result = .success(try coordinator.submit(request, capabilities: caps, decision: try OperationsSubject.decision(name))) }
                else { result = .success(try coordinator.submit(request, capabilities: caps)) }
            } catch let error as OperationError { result = .failure(error) }
            bind(world, step, op: id, result: .submit(result))
        case "retry_elevated":
            let originalID = try resolveID(world, step, "ref")
            let original = try require(coordinator.projections.first { $0.id == originalID }, "no such operation")
            let newID = OperationID(try require(step.uint64("id"), "retry_elevated needs `id`"))
            let result: Result<OperationID, OperationError>
            do {
                let retry = try elevatedRetryRequest(original, id: newID)
                result = .success(try coordinator.submitLinkedRetry(retry, capabilities: try OperationsSubject.snapshot(world, step.dict("capabilities")), original: original.id))
            } catch let error as OperationError { result = .failure(error) }
            bind(world, step, op: newID, result: .submit(result))
        case "cancel":
            let id = try resolveID(world, step, "ref")
            bind(world, step, op: nil, result: .cancel(coordinator.cancel(id).rawValue))
        default: throw RunError("operations has no action \(action)")
        }
    }

    // MARK: ports

    static func completion(_ value: JSON?) throws -> OperationCompletion {
        let value = value ?? [:]
        let effect: MutationEffect
        switch value.string("effect") ?? "unchanged" {
        case "unchanged": effect = .unchanged
        case "changed": effect = .changed
        case "partially_changed": effect = .partiallyChanged
        case "unknown": effect = .unknown
        case let other: throw RunError("unknown effect \(other)")
        }
        return OperationCompletion(effect: effect, completed: try value.strings("completed").map(location), remaining: try value.strings("remaining").map(location))
    }

    func scriptPort(_ world: DomainWorld, _ step: JSON) throws {
        switch step.string("port") {
        case "capabilities":
            let entry = inspectionsHeld[try requestIndex(step, count: backend.inspections.count)]
            let done = try require(entry.done, "that inspection was already answered")
            entry.done = nil
            let id = step.uint64("as_id").map { RequestID($0) } ?? entry.request.id
            let outcome: RequestOutcome<CapabilitySnapshot>
            if step.string("respond") == "cancelled" { outcome = .cancelled }
            else if let failed = step.dict("respond")?.dict("failed") {
                outcome = .failed(OperationError(try errorKind(try require(failed.string("kind"), "failed needs `kind`")), source: entry.request.location))
            } else if let ok = step.dict("respond")?.dict("succeeded") {
                outcome = .succeeded(try OperationsSubject.snapshot(world, ok))
            } else { throw RunError("unknown `respond`: \(step["respond"] ?? "nil")") }
            done(RequestCompletion(id: id, outcome: outcome))
        case "operations":
            let entry = backend.operations[try requestIndex(step, count: backend.operations.count)]
            let id = step.uint64("as_id").map { OperationID($0) } ?? entry.request.id
            var sequence = entry.nextSequence
            for event in try require(step.array("emit"), "operations port needs `emit`") {
                let built: OperationEvent
                if let name = event as? String {
                    switch name {
                    case "started": built = .started(id: id, sequence: sequence)
                    case "cancel_requested": built = .cancelRequested(id: id, sequence: sequence)
                    default: throw RunError("unknown event \(name)")
                    }
                } else if let map = event as? JSON, let (name, body) = map.first {
                    switch name {
                    case "progress":
                        let pair = (body as? [Any]) ?? []
                        built = .progress(id: id, sequence: sequence, progress: OperationProgress(
                            completedItems: (pair.first as? NSNumber)?.uint64Value ?? 0, totalItems: (pair.count > 1 ? pair[1] as? NSNumber : nil)?.uint64Value))
                    case "succeeded": built = .succeeded(id: id, sequence: sequence, completion: try OperationsSubject.completion(body as? JSON))
                    case "failed":
                        let b = (body as? JSON) ?? [:]
                        built = .failed(id: id, sequence: sequence, error: OperationError(try errorKind(try require(b.string("kind"), "failed needs `kind`"))), completion: try OperationsSubject.completion(b))
                    case "cancelled": built = .cancelled(id: id, sequence: sequence, outcome: CancellationOutcome(completion: try OperationsSubject.completion(body as? JSON)))
                    default: throw RunError("unknown event \(name)")
                    }
                } else { throw RunError("unknown event \(event)") }
                entry.sink(built)
                sequence += 1
            }
            entry.nextSequence = sequence
        default: throw RunError("port \(step["port"] ?? "?") is not an operations port")
        }
    }

    private var inspectionsHeld: [HeldInspection] { backend.inspections }

    func expectPortRequests(_ step: JSON) throws {
        let got = step.string("port") == "capabilities" ? backend.inspections.count : backend.operations.count
        let want = try require(step.int("count"), "port_requests needs `count`")
        if got != want { throw RunError("\(step["port"] ?? "?") port received \(got) request(s), expected \(want)") }
        if let id = step.dict("last")?.uint64("id") {
            let last = try require(backend.operations.last, "no operation dispatched")
            try expectEqual("last dispatched operation id", last.request.id.value, id)
        }
    }

    // MARK: expectations

    func expectation(_ world: DomainWorld, _ name: String, _ step: JSON) throws {
        switch name {
        case "record":
            let id = try resolveID(world, step, "ref")
            let record = try require(coordinator.projections.first { $0.id == id }, "no record for \(id)")
            if let want = step.string("state") { try expectEqual("state", record.state.name, want) }
            if let want = step.string("error") {
                var got: OperationErrorKind?
                if case .failed(let error) = record.state { got = error.kind }
                try expectEqual("error", got, try errorKind(want))
            }
            if let want = step.string("change") { try expectEqual("change", record.change.rawValue, want) }
            if step.has("completed") { try expectEqual("completed", record.completion.completed.map(\.uri), step.strings("completed")) }
            if step.has("remaining") { try expectEqual("remaining", record.completion.remaining.map(\.uri), step.strings("remaining")) }
            if step.has("events") { try expectEqual("events", record.events.map(\.name), step.strings("events")) }
            for absent in step.strings("lacks_events") where record.events.contains(where: { $0.name == absent }) {
                throw RunError("record has a \(absent) event")
            }
            if let count = step.int("events_count") { try expectEqual("event count", record.events.count, count) }
            if let count = step.int("terminal_events") { try expectEqual("terminal events", record.events.filter(\.isTerminal).count, count) }
            if step.has("retry_of") {
                let want: UInt64? = step.isNull("retry_of") ? nil : (step.string("retry_of") != nil ? try resolveID(world, step, "retry_of").value : step.uint64("retry_of"))
                try expectEqual("retry_of", record.retryOf?.value, want)
            }
        case "lock_keys":
            if step.has("active") { try expectEqual("active lock keys", coordinator.activeLockKeys, step.strings("active")) }
            if let count = step.string("active_count_is") {
                let n = coordinator.activeLockKeys.count
                switch count {
                case "zero": try expectEqual("active lock keys", n, 0)
                case "non_zero": if n == 0 { throw RunError("0 active lock keys do not satisfy non_zero") }
                default: throw RunError("\(n) active lock keys do not satisfy \(count)")
                }
            }
            if let want = step.dict("for") {
                var request = try OperationsSubject.request(want)
                request.id = OperationID(0)
                let caps = try OperationsSubject.snapshot(world, step.dict("capabilities"))
                let got = try lockKeys(request, capabilities: caps)
                try expectEqual("lock keys", got, step.strings("is"))
            }
        case "submit_result":
            let name = try require(step.string("ref"), "needs `ref`").trimmingCharacters(in: CharacterSet(charactersIn: "$"))
            guard case .submit(let result)? = world.bound[name] else { throw RunError("\(name) is not a submit result") }
            switch (step.string("is"), result) {
            case ("accepted", .success): break
            case ("refused", .failure(let error)):
                if let want = step.string("error") { try expectEqual("refusal", error.kind, try errorKind(want)) }
            case (let want, let got): throw RunError("submit result is \(got), expected \(want ?? "nil")")
            }
        case "cancel_result":
            let name = try require(step.string("ref"), "needs `ref`").trimmingCharacters(in: CharacterSet(charactersIn: "$"))
            guard case .cancel(let got)? = world.bound[name] else { throw RunError("\(name) is not a cancel result") }
            try expectEqual("cancel result", got, step.string("is") ?? "")
        case "reducer":
            let reducer = try require(self.reducer, "reducer not started")
            if let count = step.int("events") { try expectEqual("reducer events", reducer.events.count, count) }
            if let terminal = step.bool("terminal") { try expectEqual("reducer terminal", reducer.isTerminal, terminal) }
        default: throw RunError("expectation \(name) is not implemented for operations")
        }
    }

    // MARK: reducer

    func reducerAction(_ action: String, _ step: JSON) throws {
        switch action {
        case "start": reducer = OperationReducer(id: OperationID(try require(step.uint64("id"), "start needs `id`")))
        case "apply":
            let event = try require(step.dict("event"), "apply needs `event`")
            let id = OperationID(try require(event.uint64("id"), "event needs `id`"))
            let sequence = try require(event.uint64("sequence"), "event needs `sequence`")
            let built: OperationEvent
            switch event.string("kind") {
            case "started": built = .started(id: id, sequence: sequence)
            case "progress": built = .progress(id: id, sequence: sequence, progress: OperationProgress())
            case "cancel_requested": built = .cancelRequested(id: id, sequence: sequence)
            case "succeeded": built = .succeeded(id: id, sequence: sequence, completion: OperationCompletion())
            default: throw RunError("unknown reducer event \(event["kind"] ?? "nil")")
            }
            var current = try require(reducer, "reducer not started")
            let accepted = current.apply(built)
            reducer = current
            if let want = step.bool("accepted"), want != accepted { throw RunError("accepted is \(accepted), expected \(want)") }
        default: throw RunError("reducer has no action \(action)")
        }
    }
}
