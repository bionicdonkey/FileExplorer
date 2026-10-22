import Foundation

public enum OperationKind: String, Codable, Equatable {
    case copy, move, create, rename, trash, delete, compress, extract

    init(_ kind: MutationKind) { self = OperationKind(rawValue: kind.rawValue)! }

    var mutation: MutationKind? { MutationKind(rawValue: rawValue) }
}

public enum RecordState: Equatable {
    case queued, running, completed, cancelRequested
    /// The failure's message key; never backend text.
    case failed(String)
    case cancelled

    public var isActive: Bool {
        switch self {
        case .queued, .running, .cancelRequested: return true
        default: return false
        }
    }
}

public struct OperationRecord: Equatable {
    public var id: UInt64
    public var kind: OperationKind
    public var sourceURIs: [String]
    public var destinationURI: String?
    public var completed: Int
    public var total: Int
    public var state: RecordState
    public var finishedAt: Int64?
    /// A job handed over to a successor is hidden and the successor takes its label.
    public var hidden = false
    public var shownAs: OperationKind?
    public var displayKind: OperationKind { shownAs ?? kind }
}

/// What a finished operation did, kept with its record.
public struct OperationSummary: Equatable {
    public var completed: [String]
    public var remaining: [String]
    public var effect: String
    public var errorKind: String?
    public var retryOf: UInt64?
    public var explicitTarget: String?
}

/// Reports a long-running job (extraction, compression) that is not a plain filesystem mutation.
public final class JobHandle {
    public let id: UInt64
    fileprivate unowned let manager: OperationManager
    private var cancelRequested = false

    fileprivate init(id: UInt64, manager: OperationManager) { self.id = id; self.manager = manager }

    public var isCancelled: Bool { cancelRequested }
    public func cancel() { cancelRequested = true }

    public func setProgress(completed: UInt64, total: UInt64) {
        Main.runOnMain { self.manager.jobProgress(self.id, completed: completed, total: total) }
    }

    /// Ends the job. A cancelled job ends as cancelled, not failed.
    public func finish(_ result: Result<Void, JobFailure>) {
        Main.runOnMain { self.manager.jobFinished(self.id, result: result, cancelled: self.cancelRequested) }
    }
}

public struct JobFailure: Error { public var messageKey: String; public init(messageKey: String) { self.messageKey = messageKey } }

enum Main {
    static func runOnMain(_ block: @escaping () -> Void) {
        if Thread.isMainThread { block() } else { DispatchQueue.main.async(execute: block) }
    }
}

/// Owns the operation records and their persisted history. History is redacted: it keeps what was
/// done and how it ended, never a path or a file name (`file:///…`), and a record that was still
/// active when the app quit is not restored.
public final class OperationManager {
    public static let schemaVersion = 4

    private let coordinator: OperationCoordinator
    private let capabilities: CapabilitySnapshot
    private let store: URL?
    public private(set) var retentionDays: Int
    public private(set) var records: [OperationRecord] = []
    private var summaries: [UInt64: OperationSummary] = [:]
    private var tokens: [UInt64: CancellationToken] = [:]
    private var nextID: UInt64 = 1
    private var jobs: [UInt64: JobHandle] = [:]
    public private(set) var notices: [RecoverableStateNotice] = []
    /// Told whenever any record changes (for the operations page and the status bar).
    public var changed: (() -> Void)?

    public init(backend: FileOperations, capabilities: CapabilitySnapshot, historyFile: URL? = nil, retentionDays: Int = 7) {
        coordinator = OperationCoordinator(backend: backend)
        self.capabilities = capabilities
        store = historyFile
        self.retentionDays = retentionDays
        coordinator.observer = { [weak self] projection in self?.apply(projection) }
        loadHistory()
    }

    // MARK: submitting

    /// Submits a mutation. A request the coordinator refuses (an unconfirmed delete, a transfer into
    /// itself, a remote location) is recorded as a failed operation rather than thrown.
    @discardableResult
    public func submit(_ kind: MutationKind, sources: [Location], destination: Location? = nil, explicitTarget: Location? = nil,
                       decision: ConflictDecision? = nil, confirmedDelete: Bool = false) -> UInt64 {
        let id = allocateID()
        let token = CancellationToken()
        tokens[id] = token
        let request = OperationRequest(id: OperationID(id), kind: kind, sources: sources, destination: destination,
                                       explicitTarget: explicitTarget, capabilityVersion: capabilities.version, cancellation: token)
        records.append(OperationRecord(id: id, kind: OperationKind(kind), sourceURIs: sources.map(\.uri), destinationURI: destination?.uri,
                                       completed: 0, total: sources.count, state: .queued, finishedAt: nil))
        do {
            if confirmedDelete { try coordinator.submitConfirmedDelete(request, capabilities: capabilities) }
            else if let decision { try coordinator.submit(request, capabilities: capabilities, decision: decision) }
            else { try coordinator.submit(request, capabilities: capabilities) }
        } catch let error as OperationError {
            finish(id, state: .failed(error.messageKey), summary: OperationSummary(
                completed: [], remaining: sources.map(\.uri), effect: "unchanged", errorKind: error.kind.rawValue, retryOf: nil, explicitTarget: explicitTarget?.uri))
        } catch {
            finish(id, state: .failed(OperationError(.unknown).messageKey), summary: nil)
        }
        changed?()
        return id
    }

    private func allocateID() -> UInt64 {
        defer { nextID += 1 }
        return nextID
    }

    /// Starts a job that reports its own progress. The record is shown as running until it finishes.
    public func beginJob(_ kind: OperationKind, sources: [Location], destination: Location?) -> JobHandle {
        let id = allocateID()
        let handle = JobHandle(id: id, manager: self)
        jobs[id] = handle
        records.append(OperationRecord(id: id, kind: kind, sourceURIs: sources.map(\.uri), destinationURI: destination?.uri,
                                       completed: 0, total: 0, state: .running, finishedAt: nil))
        changed?()
        return handle
    }

    fileprivate func jobProgress(_ id: UInt64, completed: UInt64, total: UInt64) {
        guard let index = records.firstIndex(where: { $0.id == id }), records[index].state.isActive else { return }
        records[index].completed = Int(completed)
        records[index].total = Int(total)
        changed?()
    }

    fileprivate func jobFinished(_ id: UInt64, result: Result<Void, JobFailure>, cancelled: Bool) {
        jobs[id] = nil
        guard let index = records.firstIndex(where: { $0.id == id }), records[index].state.isActive else { return }
        switch result {
        case .success: finish(id, state: .completed, summary: nil)
        case .failure(let failure): finish(id, state: cancelled ? .cancelled : .failed(failure.messageKey), summary: nil)
        }
        changed?()
    }

    // MARK: events

    private func apply(_ projection: OperationProjection) {
        let id = projection.id.value
        guard let index = records.firstIndex(where: { $0.id == id }) else { return }
        records[index].completed = projection.completion.completed.count
        records[index].total = projection.request.sources.count
        switch projection.state {
        case .queued: records[index].state = .queued
        case .running: records[index].state = .running
        case .cancelRequested: records[index].state = .cancelRequested
        case .succeeded, .failed, .cancelled:
            let state: RecordState
            switch projection.state {
            case .succeeded: state = .completed
            case .failed(let error): state = .failed(error.messageKey)
            default: state = .cancelled
            }
            var errorKind: String?
            if case .failed(let error) = projection.state { errorKind = error.kind.rawValue }
            finish(id, state: state, summary: OperationSummary(
                completed: projection.completion.completed.map(\.uri), remaining: projection.completion.remaining.map(\.uri),
                effect: projection.completion.effect.rawValue, errorKind: errorKind, retryOf: projection.retryOf?.value,
                explicitTarget: projection.request.explicitTarget?.uri))
        }
        changed?()
    }

    private func finish(_ id: UInt64, state: RecordState, summary: OperationSummary?) {
        guard let index = records.firstIndex(where: { $0.id == id }) else { return }
        records[index].state = state
        records[index].finishedAt = Int64(Date().timeIntervalSince1970)
        if let summary { summaries[id] = summary }
        tokens[id] = nil
        persistHistory()
    }

    // MARK: queries and control

    public func summary(_ id: UInt64) -> OperationSummary? { summaries[id] }
    public var activeCount: Int { records.filter { $0.state == .running }.count }
    public var activeRecords: [OperationRecord] { records.filter { $0.state.isActive && !$0.hidden } }

    public func cancel(_ id: UInt64) {
        if let job = jobs[id] { job.cancel() }
        else { _ = coordinator.cancel(OperationID(id)) }
        if let index = records.firstIndex(where: { $0.id == id }), records[index].state.isActive, records[index].state != .queued {
            records[index].state = .cancelRequested
        }
        changed?()
    }

    public func cancelAllActive() { records.filter { $0.state.isActive }.forEach { cancel($0.id) } }

    public func clearCompleted() {
        records.removeAll { !$0.state.isActive }
        summaries = summaries.filter { id, _ in records.contains { $0.id == id } }
        persistHistory()
        changed?()
    }

    public func setRetentionDays(_ days: Int) {
        retentionDays = days
        pruneHistory()
        persistHistory()
        changed?()
    }

    // MARK: history

    private func pruneHistory() {
        let cutoff = Int64(Date().timeIntervalSince1970) - Int64(retentionDays) * 86_400
        records.removeAll { record in record.finishedAt.map { $0 < cutoff } ?? false }
        var excess = max(0, records.filter { !$0.state.isActive }.count - StateLimits.historyRecords)
        records.removeAll { record in
            if excess > 0 && !record.state.isActive { excess -= 1; return true }
            return false
        }
        summaries = summaries.filter { id, _ in records.contains { $0.id == id } }
    }

    /// Writes finished records, redacted. Active records are never written.
    public func persistHistory() {
        pruneHistory()
        guard let store else { return }
        func redact(_ uri: String) -> String { (try? Location(uri: uri))?.redacted ?? "…" }
        var out: [[String: Any]] = []
        for record in records where !record.state.isActive {
            var item: [String: Any] = [
                "id": record.id, "kind": record.kind.rawValue, "sources": record.sourceURIs.map(redact),
                "completed": record.completed, "total": record.total, "finished_at": record.finishedAt ?? 0,
            ]
            if let destination = record.destinationURI { item["destination"] = redact(destination) }
            switch record.state {
            case .completed: item["state"] = "Completed"
            case .cancelled: item["state"] = "Cancelled"
            case .failed(let key): item["state"] = "Failed"; item["failure"] = key
            default: continue
            }
            if let summary = summaries[record.id] {
                var outcome: [String: Any] = ["completed": summary.completed.map(redact), "remaining": summary.remaining.map(redact), "effect": summary.effect]
                if let kind = summary.errorKind { outcome["error_kind"] = kind }
                if let retry = summary.retryOf { outcome["retry_of"] = retry }
                item["outcome"] = outcome
            }
            out.append(item)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: ["schema_version": OperationManager.schemaVersion, "records": out], options: [.prettyPrinted, .sortedKeys]) else { return }
        try? BoundedState.write(data, to: store)
    }

    private func loadHistory() {
        guard let store, let data = try? BoundedState.read(store),
              let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let items = root["records"] as? [[String: Any]] else { return }
        for item in items.suffix(StateLimits.historyRecords) {
            guard let id = (item["id"] as? NSNumber)?.uint64Value, let kindName = item["kind"] as? String, let kind = OperationKind(rawValue: kindName),
                  let stateName = item["state"] as? String, let finished = (item["finished_at"] as? NSNumber)?.int64Value else {
                notices.append(.malformedRecord)
                continue
            }
            let state: RecordState
            switch stateName {
            case "Completed": state = .completed
            case "Cancelled": state = .cancelled
            case "Failed": state = .failed(item["failure"] as? String ?? OperationError(.unknown).messageKey)
            default: notices.append(.malformedRecord); continue   // an active record is never restored
            }
            records.append(OperationRecord(id: id, kind: kind, sourceURIs: (item["sources"] as? [String]) ?? [], destinationURI: item["destination"] as? String,
                                           completed: (item["completed"] as? NSNumber)?.intValue ?? 0, total: (item["total"] as? NSNumber)?.intValue ?? 0,
                                           state: state, finishedAt: finished))
            if let outcome = item["outcome"] as? [String: Any] {
                summaries[id] = OperationSummary(completed: (outcome["completed"] as? [String]) ?? [], remaining: (outcome["remaining"] as? [String]) ?? [],
                                                 effect: (outcome["effect"] as? String) ?? "unchanged", errorKind: outcome["error_kind"] as? String,
                                                 retryOf: (outcome["retry_of"] as? NSNumber)?.uint64Value, explicitTarget: nil)
            }
            nextID = max(nextID, id + 1)
        }
        pruneHistory()
    }
}
