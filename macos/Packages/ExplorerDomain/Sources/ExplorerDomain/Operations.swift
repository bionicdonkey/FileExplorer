import Foundation

public enum OperationState: Equatable {
    case queued, running, cancelRequested, succeeded
    case failed(OperationError)
    case cancelled(CancellationOutcome)

    public var name: String {
        switch self {
        case .queued: return "queued"
        case .running: return "running"
        case .cancelRequested: return "cancel_requested"
        case .succeeded: return "succeeded"
        case .failed: return "failed"
        case .cancelled: return "cancelled"
        }
    }
}

public enum ChangeExtent: String, Equatable { case unchanged, complete, partial, unknown }
public enum CancelResult: String, Equatable { case requested, cancelledBeforeStart = "cancelled_before_start", alreadyTerminal = "already_terminal", notFound = "not_found" }

public struct OperationProjection {
    public let id: OperationID
    public var request: OperationRequest
    public var state: OperationState = .queued
    public var change: ChangeExtent = .unchanged
    public var completion: OperationCompletion
    public var progress = OperationProgress()
    public var events: [OperationEvent] = []
    public var retryOf: OperationID?
}

private func invalid(_ request: OperationRequest) -> OperationError {
    OperationError(.invalidRequest, source: request.sources.first, destination: request.destination)
}

/// A request to redo an operation with elevation. Eligible only after a `permissionDenied`
/// failure that changed nothing, for exactly one local file source, under a new operation id.
public func elevatedRetryRequest(_ original: OperationProjection, id: OperationID) throws -> OperationRequest {
    var eligible = false
    if case .failed(let error) = original.state, error.kind == .permissionDenied,
       original.change == .unchanged, original.request.sources.count == 1, id != original.id {
        let locations = original.request.sources + [original.request.destination, original.request.explicitTarget].compactMap { $0 }
        eligible = locations.allSatisfy { location in
            location.uri.hasPrefix("file:") && !(location.localPath?.split(separator: "/").contains("gvfs") ?? true)
        }
    }
    guard eligible else { throw invalid(original.request) }
    var request = original.request
    request.id = id
    request.cancellation = CancellationToken()
    return request
}

private func isLocal(_ location: Location) -> Bool { location.uri.hasPrefix("file:") }

private func isDescendant(_ location: Location, of ancestor: Location) -> Bool {
    location != ancestor && location.isWithin(ancestor)
}

/// Lock keys serialise operations by the locations they touch. For local locations the keys are
/// every ancestor of every location involved; for any other location nothing is inferred from the
/// text of the URI, so an opaque provider serialises against everything.
public func lockKeys(_ request: OperationRequest, capabilities: CapabilitySnapshot) throws -> [String] {
    if request.sources.isEmpty || (request.explicitTarget != nil && request.sources.count != 1) { throw invalid(request) }
    var locations = request.sources
    if let destination = request.destination { locations.append(destination) }
    if let target = request.explicitTarget { locations.append(target) }
    if [.copy, .move, .rename].contains(request.kind) {
        if request.destination == nil && request.explicitTarget == nil { throw invalid(request) }
        for target in [request.destination, request.explicitTarget].compactMap({ $0 }) {
            try validateProviderTarget(request, target: target, capabilities: capabilities)
        }
        for source in request.sources {
            if let destination = request.destination {
                if isLocal(source) && isLocal(destination) {
                    if source == destination || destination.isWithin(source) { throw invalid(request) }
                } else if source == destination {
                    throw invalid(request)
                }
                if request.explicitTarget == nil {
                    let name = source.uriBasename
                    guard !name.isEmpty, let child = try? destination.child(named: name) else { throw invalid(request) }
                    locations.append(child)
                }
            }
            if let target = request.explicitTarget {
                if source == target || (isLocal(source) && isLocal(target) && target.isWithin(source)) { throw invalid(request) }
            }
        }
    }
    var keys = Set<String>()
    for location in locations {
        keys.insert(location.uri)
        if isLocal(location) {
            var parent = location.parent
            while let next = parent {
                keys.insert(next.uri)
                parent = next.parent
            }
        } else {
            keys.insert("opaque-provider:*")
        }
    }
    if case .knownAncestors(let ancestors) = capabilities.containment { keys.formUnion(ancestors.map(\.uri)) }
    return keys.sorted()
}

private func validateProviderTarget(_ request: OperationRequest, target: Location, capabilities: CapabilitySnapshot) throws {
    if request.sources.allSatisfy(isLocal) && isLocal(target) { return }
    // Without a trusted, complete ancestry snapshot, the absence of a URI prefix proves nothing:
    // aliases and opaque provider identifiers may be descendants.
    if case .knownAncestors(let ancestors) = capabilities.containment,
       request.sources.allSatisfy({ $0 != target && !ancestors.contains($0) }) { return }
    throw invalid(request)
}

extension Location {
    /// The last path component, decoded; empty for a root.
    var uriBasename: String {
        URIText.unescape(parts.path.split(separator: "/", omittingEmptySubsequences: true).last.map(String.init) ?? "")
    }
}

/// Main-thread operation scheduling. Only accepted events change projections.
public final class OperationCoordinator {
    private final class Record {
        var projection: OperationProjection
        var reducer: OperationReducer
        let locks: [String]
        var dispatched = false
        var backendSequence: UInt64?
        let decision: ConflictDecision?

        init(projection: OperationProjection, locks: [String], decision: ConflictDecision?) {
            self.projection = projection
            reducer = OperationReducer(id: projection.id)
            self.locks = locks
            self.decision = decision
        }
    }

    public static let maximumRecords = 100
    private let backend: FileOperations
    private var records: [Record] = []

    public init(backend: FileOperations) { self.backend = backend }

    @discardableResult
    public func submit(_ request: OperationRequest, capabilities: CapabilitySnapshot) throws -> OperationID {
        if request.kind == .delete { throw invalid(request) }
        return try submitInner(request, capabilities, retryOf: nil, decision: nil)
    }

    /// The caller must have obtained explicit confirmation for a permanent delete.
    @discardableResult
    public func submitConfirmedDelete(_ request: OperationRequest, capabilities: CapabilitySnapshot) throws -> OperationID {
        if request.kind != .delete { throw invalid(request) }
        return try submitInner(request, capabilities, retryOf: nil, decision: nil)
    }

    @discardableResult
    public func submitLinkedRetry(_ request: OperationRequest, capabilities: CapabilitySnapshot, original: OperationID) throws -> OperationID {
        guard let prior = projections.first(where: { $0.id == original }) else { throw invalid(request) }
        let expected = try elevatedRetryRequest(prior, id: request.id)
        if expected.sources != request.sources || expected.destination != request.destination
            || expected.explicitTarget != request.explicitTarget || expected.kind != request.kind {
            throw invalid(request)
        }
        return try submitInner(request, capabilities, retryOf: original, decision: nil)
    }

    @discardableResult
    public func submit(_ request: OperationRequest, capabilities: CapabilitySnapshot, decision: ConflictDecision) throws -> OperationID {
        if request.kind == .delete { throw invalid(request) }
        return try submitInner(request, capabilities, retryOf: nil, decision: decision)
    }

    private func submitInner(_ request: OperationRequest, _ capabilities: CapabilitySnapshot, retryOf: OperationID?, decision: ConflictDecision?) throws -> OperationID {
        let locks = try lockKeys(request, capabilities: capabilities)
        let id = request.id
        if records.contains(where: { $0.projection.id == id }) { throw invalid(request) }
        let projection = OperationProjection(id: id, request: request, completion: OperationCompletion(remaining: request.sources), retryOf: retryOf)
        let record = Record(projection: projection, locks: locks, decision: decision)
        records.append(record)
        if decision == .skip || decision == .cancelRemaining {
            let completion = record.projection.completion
            accept(decision == .skip
                   ? .succeeded(id: id, sequence: 0, completion: completion)
                   : .cancelled(id: id, sequence: 0, outcome: CancellationOutcome(completion: completion)),
                   fromBackend: false)
        } else {
            schedule()
        }
        return id
    }

    public var projections: [OperationProjection] { records.map(\.projection) }

    public var activeLockKeys: [String] {
        Set(records.filter { $0.dispatched && !$0.reducer.isTerminal }.flatMap(\.locks)).sorted()
    }

    public func cancel(_ id: OperationID) -> CancelResult {
        guard let record = records.first(where: { $0.projection.id == id }) else { return .notFound }
        if record.reducer.isTerminal { return .alreadyTerminal }
        if record.projection.state == .cancelRequested { return .requested }
        record.projection.request.cancellation.cancel()
        if record.dispatched {
            accept(.cancelRequested(id: id, sequence: 0), fromBackend: false)
            return .requested
        }
        accept(.cancelled(id: id, sequence: 0, outcome: CancellationOutcome(completion: OperationCompletion(remaining: record.projection.request.sources))),
               fromBackend: false)
        return .cancelledBeforeStart
    }

    private func schedule() {
        var occupied = Set(records.filter { $0.dispatched && !$0.reducer.isTerminal }.flatMap(\.locks))
        var ready: [OperationRequest] = []
        for record in records where !record.dispatched && !record.reducer.isTerminal {
            if record.locks.contains(where: occupied.contains) {
                occupied.formUnion(record.locks)
                continue
            }
            occupied.formUnion(record.locks)
            record.dispatched = true
            ready.append(record.projection.request)
        }
        for request in ready { inspect(request, at: 0) }
    }

    /// Inspects each source, then the destination and explicit target, one after another; then
    /// dispatches. A stale version, a missing capability or a denied inspection fails the request
    /// without work, except that a linked elevated retry tolerates a permission denial.
    private func inspect(_ request: OperationRequest, at index: Int) {
        let locations = request.sources + [request.destination, request.explicitTarget].compactMap { $0 }
        if index == locations.count {
            let decision = records.first(where: { $0.projection.id == request.id })?.decision
            let expected = request.id
            let sink: OperationSink = { [self] event in
                if event.id == expected { accept(event, fromBackend: true) }
            }
            if let decision { backend.submit(request, decision: decision, sink: sink) } else { backend.submit(request, sink: sink) }
            return
        }
        let elevated = records.first(where: { $0.projection.id == request.id })?.projection.retryOf != nil
        let capabilityRequestID = RequestID.allocate()
        let isSource = index < request.sources.count
        let needed: Capability = isSource ? request.kind.capability : .create
        backend.inspectOperationCapabilities(
            CapabilityRequest(id: capabilityRequestID, location: locations[index], cancellation: request.cancellation), capability: needed
        ) { [self] result in
            if result.id != capabilityRequestID { return }
            if !isSource, [.copy, .move, .rename].contains(request.kind), case .succeeded(let snapshot) = result.outcome {
                do { try validateProviderTarget(request, target: locations[index], capabilities: snapshot) } catch {
                    accept(.failed(id: request.id, sequence: 0, error: error as! OperationError,
                                   completion: OperationCompletion(remaining: request.sources)), fromBackend: false)
                    return
                }
            }
            switch result.outcome {
            case .succeeded(let snapshot) where snapshot.version == request.capabilityVersion && snapshot.supports(needed):
                inspect(request, at: index + 1)
            case .failed(let error) where elevated && error.kind == .permissionDenied:
                inspect(request, at: index + 1)
            case let outcome:
                let completion = OperationCompletion(remaining: request.sources)
                switch outcome {
                case .cancelled:
                    accept(.cancelled(id: request.id, sequence: 0, outcome: CancellationOutcome(completion: completion)), fromBackend: false)
                case .failed(let error):
                    accept(.failed(id: request.id, sequence: 0, error: error, completion: completion), fromBackend: false)
                case .succeeded:
                    accept(.failed(id: request.id, sequence: 0,
                                   error: OperationError(.capabilityChanged, source: request.sources.first, destination: request.destination),
                                   completion: completion), fromBackend: false)
                }
            }
        }
    }

    private func withSequence(_ event: OperationEvent, _ sequence: UInt64) -> OperationEvent {
        switch event {
        case .started(let id, _): return .started(id: id, sequence: sequence)
        case .progress(let id, _, let p): return .progress(id: id, sequence: sequence, progress: p)
        case .cancelRequested(let id, _): return .cancelRequested(id: id, sequence: sequence)
        case .succeeded(let id, _, let c): return .succeeded(id: id, sequence: sequence, completion: c)
        case .failed(let id, _, let e, let c): return .failed(id: id, sequence: sequence, error: e, completion: c)
        case .cancelled(let id, _, let o): return .cancelled(id: id, sequence: sequence, outcome: o)
        }
    }

    private func accept(_ incoming: OperationEvent, fromBackend: Bool) {
        guard let record = records.first(where: { $0.projection.id == incoming.id }) else { return }
        if record.reducer.isTerminal { return }
        if fromBackend {
            if let last = record.backendSequence, incoming.sequence <= last { return }
            record.backendSequence = incoming.sequence
        }
        // The event owner allocates sequences, so a backend cannot compete for the namespace.
        let event = withSequence(incoming, (record.reducer.events.last?.sequence ?? 0) + 1)
        guard record.reducer.apply(event) else { return }
        var p = record.projection
        switch event {
        case .started: if p.state != .cancelRequested { p.state = .running }
        case .cancelRequested: p.state = .cancelRequested
        case .progress(_, _, let progress): p.progress = progress
        case .succeeded(_, _, let completion): p.state = .succeeded; p.completion = completion
        case .failed(_, _, let error, let completion): p.state = .failed(error); p.completion = completion
        case .cancelled(_, _, let outcome): p.state = .cancelled(outcome); p.completion = outcome.completion
        }
        switch p.completion.effect {
        case .unchanged: p.change = .unchanged
        case .changed: p.change = .complete
        case .partiallyChanged: p.change = .partial
        case .unknown: p.change = .unknown
        }
        p.events.append(event)
        record.projection = p
        guard event.isTerminal else { return }
        var excess = max(0, records.filter { $0.reducer.isTerminal }.count - OperationCoordinator.maximumRecords)
        records.removeAll { record in
            if excess > 0 && record.reducer.isTerminal { excess -= 1; return true }
            return false
        }
        schedule()
    }
}
