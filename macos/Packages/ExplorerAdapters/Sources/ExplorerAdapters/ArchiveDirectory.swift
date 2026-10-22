import Darwin
import Foundation
import ExplorerDomain

/// Local archives as directories, read in-process. Serves the `archive://` locations the window
/// addresses, so tabs, breadcrumbs, history and Up work as for any folder. It is read-only: entries
/// can be opened, copied and moved out (a move copies, since the archive cannot be written back),
/// and the directory can be listed, but nothing can be created, renamed, trashed or deleted.
public final class ArchiveDirectoryRepository: DirectoryRepository {
    private let lock = NSLock()
    // Parsed archives keyed by file, invalidated by the file's modification time, so navigating a
    // large archive does not re-read it.
    private var cache: [String: (modified: Date, index: ArchiveReader.Index)] = [:]

    public init() {}

    /// Whether this adapter serves `location`: inside an archive whose file is local.
    public static func serves(_ location: Location) -> Bool { location.archiveSource?.localPath != nil }

    public static var entryCapabilities: CapabilitySnapshot {
        CapabilitySnapshot(version: 1, supported: [.open, .copy, .move], stableOrder: true, containment: .opaque)
    }

    static var directoryCapabilities: CapabilitySnapshot {
        CapabilitySnapshot(version: 1, supported: [.list], stableOrder: true, containment: .opaque)
    }

    func index(for path: String, cancel: CancellationToken) throws -> ArchiveReader.Index {
        let modified = ((try? FileManager.default.attributesOfItem(atPath: path)[.modificationDate]) as? Date) ?? .distantPast
        lock.lock()
        if let cached = cache[path], cached.modified == modified { lock.unlock(); return cached.index }
        lock.unlock()
        let index = try ArchiveReader.readIndex(path, cancel: cancel)
        lock.lock()
        cache[path] = (modified, index)
        lock.unlock()
        return index
    }

    public func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback) {
        Queues.work.async { [self] in
            let post: (RequestOutcome<DirectorySnapshot>) -> Void = { outcome in
                Main.deliver { done(RequestCompletion(id: request.id, outcome: outcome)) }
            }
            guard let archivePath = request.location.archiveSource?.localPath, let inner = request.location.archiveInnerPath else {
                return post(.failed(OperationError(.unsupported, source: request.location)))
            }
            if request.cancellation.isCancelled { return post(.cancelled) }
            if !PathInfo.exists(archivePath) { return post(.failed(OperationError(.notFound, source: request.location))) }
            let index: ArchiveReader.Index
            do { index = try self.index(for: archivePath, cancel: request.cancellation) }
            catch is OperationCancelled { return post(.cancelled) }
            catch { return post(.failed(OperationError(.unknown, source: request.location))) }
            if request.cancellation.isCancelled { return post(.cancelled) }
            if !inner.isEmpty && !index.dirs.contains(inner) { return post(.failed(OperationError(.notFound, source: request.location))) }
            var entries: [EntrySnapshot] = []
            for child in index.children(of: inner) {
                guard let location = try? request.location.child(named: child.name) else { continue }
                entries.append(EntrySnapshot(
                    location: location, displayName: child.name, kind: child.isDirectory ? .directory : .file, size: child.size, modified: child.modified,
                    contentType: ContentTypes.contentType(forName: child.name, isDirectory: child.isDirectory), capabilities: ArchiveDirectoryRepository.entryCapabilities))
            }
            post(.succeeded(DirectorySnapshot(location: ResolvedLocation(requested: request.location, resolved: request.location),
                                              entries: entries, capabilities: ArchiveDirectoryRepository.directoryCapabilities)))
        }
    }
}

/// Chooses the directory adapter for a location, by the location and not the caller, so every
/// window, sidebar and picker gets the same answer: a local archive is served in-process, a local
/// folder by the local repository, and anything else (a remote location) is unsupported.
public final class DirectoryRouter: DirectoryRepository {
    private let local = LocalDirectoryRepository()
    private let archives = ArchiveDirectoryRepository()

    public init() {}

    public func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback) { route(request.location).list(request, done: done) }
    public func listPages(_ request: DirectoryRequest, sink: @escaping DirectoryCallback) { route(request.location).listPages(request, sink: sink) }

    public func inspectVisibleMetadata(_ request: VisibleMetadataRequest, done: @escaping VisibleMetadataCallback) {
        let (inArchive, others) = (request.locations.filter(\.isInsideArchive), request.locations.filter { !$0.isInsideArchive })
        // Entries inside an archive already carry their metadata from the listing.
        if inArchive.isEmpty { return local.inspectVisibleMetadata(request, done: done) }
        let rows = inArchive.map { VisibleMetadata(location: $0, contentType: ContentTypes.contentType(forName: $0.displayName, isDirectory: false)) }
        if others.isEmpty { return Main.deliver { done(RequestCompletion(id: request.id, outcome: request.cancellation.isCancelled ? .cancelled : .succeeded(rows))) } }
        local.inspectVisibleMetadata(VisibleMetadataRequest(id: request.id, locations: others, cancellation: request.cancellation)) { completion in
            if case .succeeded(let more) = completion.outcome { done(RequestCompletion(id: request.id, outcome: .succeeded(rows + more))) } else { done(completion) }
        }
    }

    public func watch(_ location: Location, changed: @escaping () -> Void) -> WatchHandle? {
        location.isInsideArchive ? nil : DirectoryWatcher.watch(location, changed: changed)
    }

    private func route(_ location: Location) -> DirectoryRepository {
        ArchiveDirectoryRepository.serves(location) ? archives : local
    }
}
