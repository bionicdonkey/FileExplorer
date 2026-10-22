import Darwin
import Foundation
import ExplorerDomain

/// The synchronous core of local file operations. It runs on a worker queue and reports through
/// a closure; `LocalFileOperations` turns that into main-thread events.
public enum FileExecutor {
    static let capabilityVersion: UInt64 = 1

    /// What the local adapter supports before any particular path is looked at.
    public static var localCapabilities: CapabilitySnapshot {
        CapabilitySnapshot(version: capabilityVersion, supported: [.copy, .move, .create, .rename, .trash, .delete, .open],
                           stableOrder: true, containment: .knownAncestors([]))
    }

    // MARK: capability inspection

    /// The capabilities at a location: what the path allows, falling back to its parent when the
    /// path does not exist yet (the usual state of a copy target). A missing parent is `notFound`.
    static func inspectLocal(_ location: Location, required: Capability?) throws -> CapabilitySnapshot {
        guard let path = location.filePath else { throw OperationError(.unsupported, source: location) }
        var capabilities = localCapabilities
        var info = PathInfo.at(path)
        var checked = path
        if info == .missing {
            let parent = splitPath(path).parent
            if parent == path || PathInfo.at(parent) == .missing { throw OperationError(.notFound, source: location) }
            checked = parent
            info = PathInfo.at(parent)
        }
        let (parentPath, _) = splitPath(checked)
        let parentWritable = access(parentPath, W_OK | X_OK) == 0
        if access(checked, R_OK) != 0 { capabilities.supported.remove(.copy) }
        if !parentWritable || checked == "/" {
            for capability in [Capability.move, .rename, .delete, .trash] { capabilities.supported.remove(capability) }
        }
        // Creating needs the folder itself to be writable: the location when it is one, else its folder.
        let folder = info == .directory ? checked : parentPath
        if access(folder, W_OK | X_OK) != 0 { capabilities.supported.remove(.create) }
        if let required, localCapabilities.supports(required), !capabilities.supports(required) {
            throw OperationError(.permissionDenied, source: location)
        }
        return capabilities
    }

    // MARK: execution

    /// Runs one request to its terminal event. `emit` receives every event in order, with
    /// sequence numbers 1, 2, 3... Cancellation is checked between items and inside copies.
    static func execute(_ request: OperationRequest, decision: ConflictDecision?, emit: @escaping (OperationEvent) -> Void) {
        var sequence: UInt64 = 0
        func send(_ event: OperationEvent) {
            sequence += 1
            switch event {
            case .started(let id, _): emit(.started(id: id, sequence: sequence))
            case .progress(let id, _, let p): emit(.progress(id: id, sequence: sequence, progress: p))
            case .cancelRequested(let id, _): emit(.cancelRequested(id: id, sequence: sequence))
            case .succeeded(let id, _, let c): emit(.succeeded(id: id, sequence: sequence, completion: c))
            case .failed(let id, _, let e, let c): emit(.failed(id: id, sequence: sequence, error: e, completion: c))
            case .cancelled(let id, _, let o): emit(.cancelled(id: id, sequence: sequence, outcome: o))
            }
        }
        let id = request.id
        var completion = OperationCompletion(remaining: request.sources)
        func fail(_ kind: OperationErrorKind) {
            send(.failed(id: id, sequence: 0, error: OperationError(kind, source: request.sources.first, destination: request.destination), completion: completion))
        }
        func cancelled() { send(.cancelled(id: id, sequence: 0, outcome: CancellationOutcome(completion: completion))) }

        let all = request.sources + [request.destination, request.explicitTarget].compactMap { $0 }
        if !all.allSatisfy({ $0.filePath != nil }) { return fail(.unsupported) }
        if request.capabilityVersion != capabilityVersion { return fail(.capabilityChanged) }
        if (try? lockKeys(request, capabilities: localCapabilities)) == nil { return fail(.invalidRequest) }
        if request.cancellation.isCancelled { return cancelled() }
        for (index, location) in all.enumerated() {
            let needed: Capability = index < request.sources.count ? request.kind.capability : .create
            do {
                let capabilities = try inspectLocal(location, required: needed)
                if !capabilities.supports(needed) { return fail(.capabilityChanged) }
                if index >= request.sources.count, [.copy, .move, .rename].contains(request.kind) {
                    do { _ = try lockKeys(request, capabilities: capabilities) } catch let error as OperationError {
                        return send(.failed(id: id, sequence: 0, error: error, completion: completion))
                    }
                }
            } catch let error as OperationError {
                if request.cancellation.isCancelled { return cancelled() }
                return send(.failed(id: id, sequence: 0, error: error, completion: completion))
            } catch { return fail(.unknown) }
        }
        if decision == .skip { return send(.succeeded(id: id, sequence: 0, completion: completion)) }
        if decision == .cancelRemaining { return cancelled() }
        if request.cancellation.isCancelled { return cancelled() }

        send(.started(id: id, sequence: 0))
        let total: UInt64 = request.kind == .copy ? request.sources.reduce(0) { $0 + countBytes($1.filePath ?? "", request.cancellation) } : 0
        var bytes: UInt64 = 0
        for (index, source) in request.sources.enumerated() {
            if request.cancellation.isCancelled { return cancelled() }
            let sourcePath = source.filePath!
            var target: String? = request.explicitTarget?.filePath
            if target == nil, let destination = request.destination?.filePath {
                let name = splitPath(sourcePath).name
                // A root has no name a child can be given.
                if !name.isEmpty && name != "/" { target = (destination as NSString).appendingPathComponent(name) }
            }
            if decision == .keepBoth, let current = target { target = keepBoth(current) }
            var changed = false
            do {
                try perform(request.kind, sourcePath, target, request.cancellation, replace: decision == .replace, changed: &changed) { delta in
                    bytes += delta
                    send(.progress(id: id, sequence: 0, progress: OperationProgress(
                        completedItems: UInt64(index), totalItems: UInt64(request.sources.count), completedBytes: bytes, totalBytes: total)))
                }
            } catch {
                completion.effect = changed ? .unknown : (completion.completed.isEmpty ? .unchanged : .partiallyChanged)
                if error is OperationCancelled { return cancelled() }
                let kind = (error as? OperationError)?.kind ?? backendFailure(error).kind
                return send(.failed(id: id, sequence: 0,
                                    error: OperationError(kind, source: source, destination: request.destination ?? request.explicitTarget),
                                    completion: completion))
            }
            completion.completed.append(source)
            completion.remaining.removeFirst()
            completion.effect = completion.remaining.isEmpty ? .changed : .partiallyChanged
            send(.progress(id: id, sequence: 0, progress: OperationProgress(
                completedItems: UInt64(index + 1), totalItems: UInt64(request.sources.count),
                completedBytes: request.kind == .copy ? bytes : nil, totalBytes: request.kind == .copy ? total : nil)))
        }
        send(.succeeded(id: id, sequence: 0, completion: completion))
    }

    // MARK: per-item work

    static func perform(_ kind: MutationKind, _ source: String, _ target: String?, _ cancel: CancellationToken, replace: Bool,
                        changed: inout Bool, progress: (UInt64) -> Void) throws {
        if cancel.isCancelled { throw OperationCancelled() }
        if [.copy, .move, .rename].contains(kind) {
            guard let target else { throw OperationError(.invalidRequest) }
            // Logical identity is preserved, but physical containment must also reject a
            // destination that is an alias back into the source tree.
            if let realSource = realpathString(source), let realTarget = resolvedTarget(target),
               realTarget == realSource || realTarget.hasPrefix(realSource.hasSuffix("/") ? realSource : realSource + "/") {
                throw OperationError(.invalidRequest)
            }
            if !replace && PathInfo.exists(target) { throw POSIXFailure(EEXIST) }
        }
        switch kind {
        case .copy:
            try copyTree(source, target!, cancel, replace: replace, changed: &changed, progress: progress)
        case .move, .rename:
            if PathInfo.at(source) == .missing { throw POSIXFailure(ENOENT) }
            changed = true
            try move(source, target!, replace: replace, cancel: cancel, progress: progress)
        case .create:
            if mkdir(source, 0o777) != 0 { throw POSIXFailure() }
            changed = true
        case .trash:
            if PathInfo.at(source) == .missing { throw POSIXFailure(ENOENT) }
            changed = true
            do { try FileManager.default.trashItem(at: URL(fileURLWithPath: source), resultingItemURL: nil) }
            catch { throw POSIXFailure(posixCode(of: error) ?? EIO) }
        case .delete:
            try deleteTree(source, cancel, changed: &changed)
        }
    }

    private static func posixCode(of error: Error) -> Int32? {
        let ns = error as NSError
        if ns.domain == NSPOSIXErrorDomain { return Int32(ns.code) }
        if let underlying = ns.userInfo[NSUnderlyingErrorKey] as? NSError, underlying.domain == NSPOSIXErrorDomain { return Int32(underlying.code) }
        switch ns.code {
        case NSFileNoSuchFileError, NSFileReadNoSuchFileError: return ENOENT
        case NSFileWriteNoPermissionError, NSFileReadNoPermissionError: return EACCES
        case NSFileWriteVolumeReadOnlyError: return EROFS
        default: return nil
        }
    }

    private static func realpathString(_ path: String) -> String? { realPath(path) }

    /// The target with its existing part resolved: itself, or its parent plus the new name.
    private static func resolvedTarget(_ target: String) -> String? {
        if let real = realpathString(target) { return real }
        let (parent, name) = splitPath(target)
        guard let realParent = realpathString(parent) else { return nil }
        return (realParent as NSString).appendingPathComponent(name)
    }

    /// Renames atomically where possible; across volumes it copies, then deletes the source.
    private static func move(_ source: String, _ target: String, replace: Bool, cancel: CancellationToken, progress: (UInt64) -> Void) throws {
        let result = replace ? rename(source, target) : renamex_np(source, target, UInt32(RENAME_EXCL))
        if result == 0 { return }
        let code = errno
        guard code == EXDEV else { throw POSIXFailure(code) }
        var changed = false
        try copyTree(source, target, cancel, replace: replace, changed: &changed, progress: progress)
        var deleted = false
        try deleteTree(source, cancel, changed: &deleted)
    }

    /// The first free `stem (n).ext`, counting up from 2, split at the last dot.
    static func keepBoth(_ target: String) -> String {
        if !PathInfo.exists(target) { return target }
        let (parent, name) = splitPath(target)
        var stem = name, ext = ""
        if let dot = name.lastIndex(of: "."), dot > name.startIndex {
            stem = String(name[..<dot])
            ext = String(name[name.index(after: dot)...])
        }
        var n = 2
        while true {
            let candidate = (parent as NSString).appendingPathComponent(ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)")
            if !PathInfo.exists(candidate) { return candidate }
            n += 1
        }
    }

    // MARK: trees

    static func countBytes(_ path: String, _ cancel: CancellationToken) -> UInt64 {
        var status = stat()
        guard lstat(path, &status) == 0 else { return 0 }
        if status.st_mode & S_IFMT != S_IFDIR { return UInt64(max(0, status.st_size)) }
        var total: UInt64 = 0
        for name in (try? FileManager.default.contentsOfDirectory(atPath: path)) ?? [] {
            if cancel.isCancelled { break }
            total += countBytes((path as NSString).appendingPathComponent(name), cancel)
        }
        return total
    }

    static func copyTree(_ source: String, _ target: String, _ cancel: CancellationToken, replace: Bool, changed: inout Bool, progress: (UInt64) -> Void) throws {
        if cancel.isCancelled { throw OperationCancelled() }
        if PathInfo.at(source) == .directory {
            if mkdir(target, 0o777) != 0 { throw POSIXFailure() }
            changed = true
            for name in try FileManager.default.contentsOfDirectory(atPath: source) {
                try copyTree((source as NSString).appendingPathComponent(name), (target as NSString).appendingPathComponent(name), cancel,
                             replace: replace, changed: &changed, progress: progress)
            }
            // Directory metadata (permissions, dates) is applied after its contents exist.
            _ = copyfile(source, target, nil, copyfile_flags_t(COPYFILE_METADATA | COPYFILE_NOFOLLOW))
            return
        }
        changed = true
        try copyFile(source, target, cancel, replace: replace, progress: progress)
    }

    private final class CopyContext {
        let cancel: CancellationToken
        let progress: (UInt64) -> Void
        var previous: Int64 = 0
        init(cancel: CancellationToken, progress: @escaping (UInt64) -> Void) { self.cancel = cancel; self.progress = progress }
    }

    /// One file (or symbolic link, copied as a link) with its metadata, reporting byte progress
    /// and stopping promptly when cancelled.
    static func copyFile(_ source: String, _ target: String, _ cancel: CancellationToken, replace: Bool, progress: (UInt64) -> Void) throws {
        try withoutActuallyEscaping(progress) { escapable in
            try copyFile(source, target, cancel, replace: replace, escapableProgress: escapable)
        }
    }

    private static func copyFile(_ source: String, _ target: String, _ cancel: CancellationToken, replace: Bool, escapableProgress progress: @escaping (UInt64) -> Void) throws {
        let context = CopyContext(cancel: cancel, progress: progress)
        let state = copyfile_state_alloc()
        defer { copyfile_state_free(state) }
        let callback: copyfile_callback_t = { what, stage, state, _, _, raw in
            guard let raw else { return COPYFILE_CONTINUE }
            let context = Unmanaged<CopyContext>.fromOpaque(raw).takeUnretainedValue()
            if context.cancel.isCancelled { return COPYFILE_QUIT }
            if what == COPYFILE_COPY_DATA, stage == COPYFILE_PROGRESS {
                var copied: off_t = 0
                copyfile_state_get(state, UInt32(COPYFILE_STATE_COPIED), &copied)
                let delta = Int64(copied) - context.previous
                if delta > 0 { context.progress(UInt64(delta)) }
                context.previous = Int64(copied)
            }
            return COPYFILE_CONTINUE
        }
        let unmanaged = Unmanaged.passRetained(context)
        defer { unmanaged.release() }
        copyfile_state_set(state, UInt32(COPYFILE_STATE_STATUS_CB), unsafeBitCast(callback, to: UnsafeRawPointer.self))
        copyfile_state_set(state, UInt32(COPYFILE_STATE_STATUS_CTX), unmanaged.toOpaque())
        var flags = copyfile_flags_t(COPYFILE_ALL | COPYFILE_NOFOLLOW)
        if !replace { flags |= copyfile_flags_t(COPYFILE_EXCL) }
        if copyfile(source, target, state, flags) != 0 {
            let code = errno
            if cancel.isCancelled { _ = unlink(target); throw OperationCancelled() }
            throw POSIXFailure(code)
        }
        // A file smaller than one progress interval still reports its bytes.
        var status = stat()
        if lstat(source, &status) == 0, status.st_mode & S_IFMT == S_IFREG, context.previous == 0, status.st_size > 0 { progress(UInt64(status.st_size)) }
    }

    static func deleteTree(_ path: String, _ cancel: CancellationToken, changed: inout Bool) throws {
        if cancel.isCancelled { throw OperationCancelled() }
        switch PathInfo.at(path) {
        case .missing: throw POSIXFailure(ENOENT)
        case .directory:
            for name in try FileManager.default.contentsOfDirectory(atPath: path) {
                try deleteTree((path as NSString).appendingPathComponent(name), cancel, changed: &changed)
            }
            if rmdir(path) != 0 { throw POSIXFailure() }
        default:
            if unlink(path) != 0 { throw POSIXFailure() }
        }
        changed = true
    }
}

/// The local `FileOperations` adapter: inspection and execution on a worker queue, answered on the
/// main thread.
public final class LocalFileOperations: FileOperations {
    public init() {}

    public func inspectCapabilities(_ request: CapabilityRequest, done: @escaping CapabilityCallback) {
        inspect(request, required: nil, done: done)
    }

    public func inspectOperationCapabilities(_ request: CapabilityRequest, capability: Capability, done: @escaping CapabilityCallback) {
        inspect(request, required: capability, done: done)
    }

    private func inspect(_ request: CapabilityRequest, required: Capability?, done: @escaping CapabilityCallback) {
        Queues.work.async {
            let outcome: RequestOutcome<CapabilitySnapshot>
            if request.cancellation.isCancelled { outcome = .cancelled }
            else {
                do { outcome = .succeeded(try FileExecutor.inspectLocal(request.location, required: required)) }
                catch let error as OperationError { outcome = request.cancellation.isCancelled ? .cancelled : .failed(error) }
                catch { outcome = .failed(OperationError(.unknown, source: request.location)) }
            }
            Main.deliver { done(RequestCompletion(id: request.id, outcome: outcome)) }
        }
    }

    public func submit(_ request: OperationRequest, sink: @escaping OperationSink) { run(request, decision: nil, sink: sink) }

    public func submit(_ request: OperationRequest, decision: ConflictDecision, sink: @escaping OperationSink) {
        run(request, decision: decision, sink: sink)
    }

    private func run(_ request: OperationRequest, decision: ConflictDecision?, sink: @escaping OperationSink) {
        Queues.work.async {
            // The terminal event is held back until every earlier event has been delivered, so the
            // stream always ends with exactly one terminal element, last.
            FileExecutor.execute(request, decision: decision) { event in Main.deliver { sink(event) } }
        }
    }
}
