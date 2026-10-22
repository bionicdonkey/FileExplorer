import Darwin
import Foundation
import ExplorerDomain

/// The directory repository for local folders. Listings are answered on the main thread from work
/// done on a worker queue, in pages of 128 so the first page can paint before enumeration ends.
public final class LocalDirectoryRepository: DirectoryRepository {
    public static let pageSize = 128

    public init() {}

    // MARK: listing

    public func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback) {
        enumerate(request, paged: false) { done($0) }
    }

    public func listPages(_ request: DirectoryRequest, sink: @escaping DirectoryCallback) {
        enumerate(request, paged: true, sink)
    }

    private func enumerate(_ request: DirectoryRequest, paged: Bool, _ deliver: @escaping DirectoryCallback) {
        Queues.work.async {
            let post: (RequestOutcome<DirectorySnapshot>) -> Void = { outcome in
                Main.deliver { deliver(RequestCompletion(id: request.id, outcome: outcome)) }
            }
            // The system does not promise a stable order between enumerations, so offset paging is
            // refused rather than silently losing rows.
            if request.page.offset != 0 || request.page.limit != Int.max {
                return post(.failed(OperationError(.unsupported, source: request.location)))
            }
            guard let path = request.location.localPath else { return post(.failed(OperationError(.unsupported, source: request.location))) }
            if request.cancellation.isCancelled { return post(.cancelled) }
            let info = PathInfo.at(path)
            var isDirectory = info == .directory
            if info == .symlink { var s = stat(); isDirectory = stat(path, &s) == 0 && s.st_mode & S_IFMT == S_IFDIR }
            if info == .missing { return post(.failed(OperationError(.notFound, source: request.location))) }
            if !isDirectory { return post(.failed(OperationError(.unknown, source: request.location))) }
            if access(path, R_OK) != 0 { return post(.failed(OperationError(.permissionDenied, source: request.location))) }
            let names: [String]
            do { names = try FileManager.default.contentsOfDirectory(atPath: path) }
            catch { return post(.failed(OperationError(failure: backendFailure(error), source: request.location))) }

            let capabilities = LocalDirectoryRepository.directoryCapabilities(path)
            let resolved = ResolvedLocation(requested: request.location, resolved: request.location)
            let parentWritable = access(path, W_OK | X_OK) == 0
            var page: [EntrySnapshot] = []
            var offset = 0
            for name in names {
                if request.cancellation.isCancelled { return post(.cancelled) }
                page.append(LocalDirectoryRepository.entry(directory: path, name: name, parentWritable: parentWritable))
                if paged, page.count == LocalDirectoryRepository.pageSize {
                    offset += page.count
                    post(.succeeded(DirectorySnapshot(location: resolved, entries: page, capabilities: capabilities,
                                                      nextPage: PageRequest(offset: offset, limit: LocalDirectoryRepository.pageSize))))
                    page = []
                }
            }
            post(.succeeded(DirectorySnapshot(location: resolved, entries: page, capabilities: capabilities, nextPage: nil)))
        }
    }

    /// What the folder itself allows: always listable; creating needs it writable; renaming it
    /// needs its parent writable. The order is not stable.
    static func directoryCapabilities(_ path: String) -> CapabilitySnapshot {
        var supported: Set<Capability> = [.open, .list]
        if access(path, R_OK) == 0 { supported.insert(.copy) }
        if access(path, W_OK | X_OK) == 0 { supported.insert(.create) }
        if access(splitPath(path).parent, W_OK | X_OK) == 0, path != "/" { supported.formUnion([.move, .rename, .trash, .delete]) }
        return CapabilitySnapshot(version: 1, supported: supported, stableOrder: false, containment: .knownAncestors([]))
    }

    /// Identity, kind and access rights, but not size, date or content type: those arrive with
    /// visible-row metadata, so a listing never waits on them.
    static func entry(directory: String, name: String, parentWritable: Bool) -> EntrySnapshot {
        let path = (directory as NSString).appendingPathComponent(name)
        var link = stat()
        var followed = stat()
        let isLink = lstat(path, &link) == 0 && link.st_mode & S_IFMT == S_IFLNK
        let kind: EntryKind
        if stat(path, &followed) == 0 {
            switch followed.st_mode & S_IFMT {
            case S_IFDIR: kind = .directory
            case S_IFREG: kind = .file
            default: kind = .other
            }
        } else {
            // A broken link is still shown, as a file.
            kind = isLink ? .file : .other
        }
        var supported: Set<Capability> = [.open]
        if access(path, R_OK) == 0 { supported.insert(.copy) }
        if parentWritable { supported.formUnion([.move, .rename, .trash, .delete]) }
        if kind == .directory, access(path, W_OK | X_OK) == 0 { supported.insert(.create) }
        return EntrySnapshot(location: Location(localPath: path), displayName: name, kind: kind,
                             capabilities: CapabilitySnapshot(version: 1, supported: supported))
    }

    // MARK: visible metadata

    public func inspectVisibleMetadata(_ request: VisibleMetadataRequest, done: @escaping VisibleMetadataCallback) {
        Queues.work.async {
            let outcome: RequestOutcome<[VisibleMetadata]>
            if request.cancellation.isCancelled { outcome = .cancelled }
            else {
                var rows: [VisibleMetadata] = []
                for location in request.locations {
                    if request.cancellation.isCancelled { break }
                    guard let path = location.localPath else { continue }
                    var status = stat()
                    guard stat(path, &status) == 0 else { rows.append(VisibleMetadata(location: location)); continue }
                    let isDirectory = status.st_mode & S_IFMT == S_IFDIR
                    rows.append(VisibleMetadata(
                        location: location, size: isDirectory ? nil : UInt64(max(0, status.st_size)),
                        modified: Date(timeIntervalSince1970: TimeInterval(status.st_mtimespec.tv_sec)),
                        contentType: ContentTypes.contentType(forName: splitPath(path).name, isDirectory: isDirectory)))
                }
                outcome = request.cancellation.isCancelled ? .cancelled : .succeeded(rows)
            }
            Main.deliver { done(RequestCompletion(id: request.id, outcome: outcome)) }
        }
    }
}
