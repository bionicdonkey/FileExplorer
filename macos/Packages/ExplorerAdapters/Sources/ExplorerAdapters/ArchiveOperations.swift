import Darwin
import Foundation
import ExplorerDomain

/// Taking entries out of an archive.
///
/// An archive shown in the window is read by libarchive, so a plain file copy has no file to read:
/// `archive://` locations are only names this application understands. This adapter sits behind the
/// same `FileOperations` port and, when a copy or move starts inside an archive, unpacks the
/// selected entries into a scratch folder beside the destination and moves them into place.
/// Everything else goes to the wrapped adapter unchanged.
///
/// Archives are read-only, so moving an entry out copies it: the archive keeps its copy. A name
/// clash is reported before anything is unpacked, so the queue can ask what to do. Decisions,
/// progress and cancellation behave as for any transfer.
public final class ArchiveFileOperations: FileOperations {
    private let inner: FileOperations

    public init(wrapping inner: FileOperations) { self.inner = inner }

    private func handles(_ request: OperationRequest) -> Bool {
        [.copy, .move].contains(request.kind)
            && !request.sources.isEmpty
            && request.sources.allSatisfy(ArchiveDirectoryRepository.serves)
            && (request.destination.map { $0.localPath != nil } ?? (request.explicitTarget?.localPath != nil))
    }

    public func inspectCapabilities(_ request: CapabilityRequest, done: @escaping CapabilityCallback) {
        inspectOperationCapabilities(request, capability: .copy, done: done)
    }

    public func inspectOperationCapabilities(_ request: CapabilityRequest, capability: Capability, done: @escaping CapabilityCallback) {
        guard request.location.isInsideArchive else { return inner.inspectOperationCapabilities(request, capability: capability, done: done) }
        Queues.work.async {
            let outcome: RequestOutcome<CapabilitySnapshot>
            if request.cancellation.isCancelled { outcome = .cancelled }
            else if let path = request.location.archiveSource?.localPath, PathInfo.exists(path) { outcome = .succeeded(ArchiveDirectoryRepository.entryCapabilities) }
            else { outcome = .failed(OperationError(request.location.archiveSource?.localPath == nil ? .unsupported : .notFound, source: request.location)) }
            Main.deliver { done(RequestCompletion(id: request.id, outcome: outcome)) }
        }
    }

    public func submit(_ request: OperationRequest, sink: @escaping OperationSink) {
        if handles(request) { run(request, decision: nil, sink: sink) } else { inner.submit(request, sink: sink) }
    }

    public func submit(_ request: OperationRequest, decision: ConflictDecision, sink: @escaping OperationSink) {
        if handles(request) { run(request, decision: decision, sink: sink) } else { inner.submit(request, decision: decision, sink: sink) }
    }

    private struct Source { var archive: String; var inner: String; var name: String }

    private func run(_ request: OperationRequest, decision: ConflictDecision?, sink: @escaping OperationSink) {
        Queues.work.async {
            var sequence: UInt64 = 0
            func send(_ event: OperationEvent) {
                sequence += 1
                let numbered: OperationEvent
                switch event {
                case .started(let id, _): numbered = .started(id: id, sequence: sequence)
                case .progress(let id, _, let p): numbered = .progress(id: id, sequence: sequence, progress: p)
                case .cancelRequested(let id, _): numbered = .cancelRequested(id: id, sequence: sequence)
                case .succeeded(let id, _, let c): numbered = .succeeded(id: id, sequence: sequence, completion: c)
                case .failed(let id, _, let e, let c): numbered = .failed(id: id, sequence: sequence, error: e, completion: c)
                case .cancelled(let id, _, let o): numbered = .cancelled(id: id, sequence: sequence, outcome: o)
                }
                Main.deliver { sink(numbered) }
            }
            ArchiveFileOperations.execute(request, decision: decision, send: send)
        }
    }

    private static func execute(_ request: OperationRequest, decision: ConflictDecision?, send: (OperationEvent) -> Void) {
        let id = request.id
        var completion = OperationCompletion(remaining: request.sources)
        func fail(_ kind: OperationErrorKind) {
            send(.failed(id: id, sequence: 0, error: OperationError(kind, source: request.sources.first, destination: request.destination), completion: completion))
        }
        func cancelled() { send(.cancelled(id: id, sequence: 0, outcome: CancellationOutcome(completion: completion))) }

        if request.capabilityVersion != 1 { return fail(.capabilityChanged) }
        var sources: [Source] = []
        for location in request.sources {
            guard let archive = location.archiveSource?.localPath, let innerPath = location.archiveInnerPath, !innerPath.isEmpty else { return fail(.invalidRequest) }
            sources.append(Source(archive: archive, inner: innerPath, name: ArchiveReader.name(of: innerPath)))
        }
        var targets: [String] = []
        if let explicit = request.explicitTarget?.localPath {
            guard sources.count == 1 else { return fail(.invalidRequest) }
            targets = [explicit]
        } else if let destination = request.destination?.localPath {
            targets = sources.map { (destination as NSString).appendingPathComponent($0.name) }
        } else { return fail(.invalidRequest) }
        let workspaceParent = request.explicitTarget != nil ? splitPath(targets[0]).parent : request.destination!.localPath!
        if decision == .skip { return send(.succeeded(id: request.id, sequence: 0, completion: completion)) }
        if decision == .cancelRemaining { return cancelled() }
        // A name clash is reported before any work, so the queue can ask what to do without having
        // unpacked anything. KeepBoth and Replace already answer it.
        if decision == nil, targets.contains(where: PathInfo.exists) { return fail(.conflict) }
        if request.cancellation.isCancelled { return cancelled() }

        send(.started(id: id, sequence: 0))
        let totalItems = UInt64(sources.count)
        let totalBytes = sources.reduce(UInt64(0)) { $0 + UInt64((try? FileManager.default.attributesOfItem(atPath: $1.archive)[.size] as? UInt64) ?? 0) }
        send(.progress(id: id, sequence: 0, progress: OperationProgress(completedItems: 0, totalItems: totalItems, completedBytes: 0, totalBytes: totalBytes)))

        // Each archive is unpacked once, however many entries are taken from it.
        var order: [String] = []
        var groups: [String: [Int]] = [:]
        for (index, source) in sources.enumerated() {
            if groups[source.archive] == nil { order.append(source.archive) }
            groups[source.archive, default: []].append(index)
        }
        func effectAfter() -> MutationEffect { completion.completed.isEmpty ? .unchanged : .partiallyChanged }
        for archive in order {
            let scratch = (workspaceParent as NSString).appendingPathComponent(".morin-unpack-\(UUID().uuidString)")
            do { try FileManager.default.createDirectory(atPath: scratch, withIntermediateDirectories: true) }
            catch { completion.effect = effectAfter(); return fail(.unknown) }
            defer { try? FileManager.default.removeItem(atPath: scratch) }
            let wanted = groups[archive] ?? []
            let selected = wanted.map { sources[$0].inner }
            do {
                try ArchiveReader.extract(archive, to: scratch, include: { path in selected.contains { path == $0 || path.hasPrefix($0 + "/") } },
                                          cancel: request.cancellation) { _, _ in }
            } catch is OperationCancelled { completion.effect = effectAfter(); return cancelled() }
            catch {
                completion.effect = effectAfter()
                return fail(PathInfo.exists(archive) ? .unknown : .notFound)
            }
            for index in wanted {
                if request.cancellation.isCancelled { completion.effect = effectAfter(); return cancelled() }
                let from = (scratch as NSString).appendingPathComponent(sources[index].inner)
                var target = targets[index]
                if decision == .keepBoth, PathInfo.exists(target) { target = FileExecutor.keepBoth(target) }
                if decision == .replace, PathInfo.exists(target) { try? FileManager.default.removeItem(atPath: target) }
                if rename(from, target) != 0 {
                    completion.effect = effectAfter()
                    return fail(errno == ENOENT ? .notFound : .unknown)
                }
                completion.completed.append(request.sources[index])
                completion.remaining.removeAll { $0 == request.sources[index] }
                completion.effect = completion.remaining.isEmpty ? .changed : .partiallyChanged
                send(.progress(id: id, sequence: 0, progress: OperationProgress(
                    completedItems: UInt64(completion.completed.count), totalItems: totalItems, completedBytes: totalBytes, totalBytes: totalBytes)))
            }
        }
        completion.effect = .changed
        send(.succeeded(id: id, sequence: 0, completion: completion))
    }
}
