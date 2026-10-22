import Darwin
import Foundation
import ExplorerDomain

/// Search over local folders.
///
/// Filename search walks the tree itself, breadth first, so depth, the result limit, the time
/// limit and the rule that a symbolic link is never followed hold exactly. Content search needs an
/// index, so it asks Spotlight (`NSMetadataQuery`); where the root is not in the index (indexing
/// off, or the folder excluded) the answer is an explicit `unsupported`, never an empty result.
public final class LocalSearchRepository: SearchRepository {
    private var spotlight: [ObjectIdentifier: NSMetadataQuery] = [:]

    public init() {}

    public func search(_ request: SearchRequest, done: @escaping SearchCallback) {
        if request.mode == .content, request.scope.root.localPath != nil, !request.cancellation.isCancelled {
            return contentSearch(request, done: done)
        }
        Queues.work.async {
            let outcome = LocalSearchRepository.filenameSearch(request)
            Main.deliver { done(SearchCompletion(id: request.id, outcome: outcome)) }
        }
    }

    private static func completed(_ request: SearchRequest, _ matches: [EntrySnapshot]) -> SearchOutcome {
        // Every declared bound is active even when the walk happened to finish early.
        .completed(matches: matches, declaredScope: request.scope, scopeLimited: true, limits: [.depth, .resultCount, .time])
    }

    private static func incomplete(_ request: SearchRequest, _ matches: [EntrySnapshot], _ reason: SearchIncompleteReason, _ error: OperationError? = nil) -> SearchOutcome {
        .incomplete(matches: matches, reason: reason, declaredScope: request.scope, retry: .afterRefresh, error: error)
    }

    private static func unsupported(_ request: SearchRequest) -> SearchOutcome {
        .unsupported(declaredScope: request.scope, error: OperationError(.unsupported, source: request.scope.root))
    }

    static func filenameSearch(_ request: SearchRequest) -> SearchOutcome {
        let start = Date()
        var matches: [EntrySnapshot] = []
        if request.cancellation.isCancelled { return incomplete(request, matches, .cancelled) }
        guard request.mode == .filename, let rootPath = request.scope.root.localPath else { return unsupported(request) }
        var queue: [(path: String, depth: UInt32)] = [(rootPath, 0)]
        var visited = Set<String>()
        var accepted = Set<String>()
        let needle = request.query.lowercased()
        func timedOut() -> Bool { Date().timeIntervalSince(start) >= request.scope.timeout }
        while !queue.isEmpty {
            let (directory, depth) = queue.removeFirst()
            if request.cancellation.isCancelled { return incomplete(request, matches, .cancelled) }
            if timedOut() || matches.count >= request.scope.maxResults { return completed(request, matches) }
            if depth >= request.scope.maxDepth || !visited.insert(directory).inserted { continue }
            // The system's enumeration order is unstable, so a folder's children are buffered in
            // canonical order before anything from it is accepted.
            let names: [String]
            do { names = try FileManager.default.contentsOfDirectory(atPath: directory) }
            catch {
                let kind = backendFailure(error).kind
                let reason: SearchIncompleteReason = request.cancellation.isCancelled ? .cancelled
                    : (kind == .permissionDenied || kind == .mountUnavailable ? .accessLost : .providerFailure)
                return incomplete(request, matches, reason, OperationError(kind, source: request.scope.root))
            }
            let children = names.map { (name: $0, location: Location(localPath: (directory as NSString).appendingPathComponent($0))) }
                .sorted { $0.location.uri < $1.location.uri }
            for (name, child) in children {
                if request.cancellation.isCancelled { return incomplete(request, matches, .cancelled) }
                if timedOut() || matches.count >= request.scope.maxResults { return completed(request, matches) }
                let path = child.localPath!
                var link = stat()
                guard lstat(path, &link) == 0 else { continue }
                let kindBits = link.st_mode & S_IFMT
                // Symbolic links are leaves, and a filesystem boundary never resets the depth.
                if kindBits == S_IFDIR && !LocalSearchRepository.isMountPoint(path, link) { queue.append((path, depth + 1)) }
                if name.lowercased().contains(needle), accepted.insert(child.uri).inserted {
                    var followed = stat()
                    let isDirectory = stat(path, &followed) == 0 && followed.st_mode & S_IFMT == S_IFDIR
                    let entry = EntrySnapshot(
                        location: child, displayName: name, kind: isDirectory ? .directory : .file,
                        size: isDirectory ? nil : UInt64(max(0, link.st_size)),
                        modified: Date(timeIntervalSince1970: TimeInterval(link.st_mtimespec.tv_sec)),
                        contentType: ContentTypes.contentType(forName: name, isDirectory: isDirectory),
                        capabilities: LocalDirectoryRepository.entry(directory: directory, name: name, parentWritable: access(directory, W_OK | X_OK) == 0).capabilities)
                    if request.cancellation.isCancelled { return incomplete(request, matches, .cancelled) }
                    if timedOut() { return completed(request, matches) }
                    matches.append(entry)
                }
            }
        }
        return request.cancellation.isCancelled ? incomplete(request, matches, .cancelled) : completed(request, matches)
    }

    private static func isMountPoint(_ path: String, _ status: stat) -> Bool {
        var parent = stat()
        return lstat((path as NSString).deletingLastPathComponent, &parent) == 0 && parent.st_dev != status.st_dev
    }

    // MARK: content (Spotlight)

    /// Whether Spotlight can be expected to have this folder in its index: not under a location
    /// Spotlight never indexes, not marked `.metadata_never_index` (here or in an ancestor), and
    /// on a volume whose indexing is enabled. Blocks briefly (it asks `mdutil`), so call it off the
    /// main thread.
    static func isIndexed(_ path: String) -> Bool {
        // realpath, not URL.resolvingSymlinksInPath: Foundation strips /private from /var and /tmp.
        let resolved = realPath(path) ?? path
        for excluded in ["/private/var", "/private/tmp", "/System", "/Library/Caches", "/cores"] where resolved == excluded || resolved.hasPrefix(excluded + "/") {
            return false
        }
        var directory = resolved
        while directory != "/" {
            if PathInfo.exists((directory as NSString).appendingPathComponent(".metadata_never_index")) { return false }
            directory = (directory as NSString).deletingLastPathComponent
        }
        var fs = statfs()
        guard statfs(resolved, &fs) == 0 else { return false }
        let mountPoint = withUnsafePointer(to: &fs.f_mntonname) { $0.withMemoryRebound(to: CChar.self, capacity: Int(MAXPATHLEN)) { String(cString: $0) } }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/mdutil")
        process.arguments = ["-s", mountPoint]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        do { try process.run() } catch { return false }
        let output = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        process.waitUntilExit()
        return output.contains("Indexing enabled")
    }

    private func contentSearch(_ request: SearchRequest, done: @escaping SearchCallback) {
        let root = request.scope.root.localPath!
        Queues.work.async { [self] in
            let indexed = !request.cancellation.isCancelled && LocalSearchRepository.isIndexed(root)
            Main.deliver { [self] in
                if request.cancellation.isCancelled {
                    return done(SearchCompletion(id: request.id, outcome: LocalSearchRepository.incomplete(request, [], .cancelled)))
                }
                guard indexed else {
                    return done(SearchCompletion(id: request.id, outcome: LocalSearchRepository.unsupported(request)))
                }
                runSpotlightQuery(request, root: root, done: done)
            }
        }
    }

    private func runSpotlightQuery(_ request: SearchRequest, root: String, done: @escaping SearchCallback) {
        do {
            let query = NSMetadataQuery()
            let key = ObjectIdentifier(query)
            spotlight[key] = query
            query.predicate = NSPredicate(format: "kMDItemTextContent CONTAINS[cd] %@", request.query)
            query.searchScopes = [root]
            var observer: NSObjectProtocol?
            var finished = false
            let finish: (SearchOutcome) -> Void = { [weak self] outcome in
                if finished { return }
                finished = true
                query.stop()
                if let observer { NotificationCenter.default.removeObserver(observer) }
                self?.spotlight[key] = nil
                done(SearchCompletion(id: request.id, outcome: outcome))
            }
            observer = NotificationCenter.default.addObserver(forName: .NSMetadataQueryDidFinishGathering, object: query, queue: .main) { _ in
                var matches: [EntrySnapshot] = []
                for case let item as NSMetadataItem in query.results {
                    guard let path = item.value(forAttribute: NSMetadataItemPathKey) as? String, path.hasPrefix(root) else { continue }
                    let relative = path.dropFirst(root.count).split(separator: "/").count
                    if UInt32(relative) > request.scope.maxDepth { continue }
                    let location = Location(localPath: path)
                    matches.append(EntrySnapshot(location: location, displayName: location.displayName, kind: .file))
                    if matches.count >= request.scope.maxResults { break }
                }
                matches.sort { $0.location.uri < $1.location.uri }
                finish(LocalSearchRepository.completed(request, matches))
            }
            request.cancellation.onCancel { Main.deliver { finish(LocalSearchRepository.incomplete(request, [], .cancelled)) } }
            // A query that has not finished by the deadline stops and reports what it has.
            DispatchQueue.main.asyncAfter(deadline: .now() + request.scope.timeout) {
                finish(LocalSearchRepository.incomplete(request, [], .providerFailure))
            }
            if !query.start() { finish(LocalSearchRepository.incomplete(request, [], .providerFailure)) }
        }
    }
}
