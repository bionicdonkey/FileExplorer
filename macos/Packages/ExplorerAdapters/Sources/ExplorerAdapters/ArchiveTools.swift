import CArchive
import Darwin
import Foundation
import ExplorerDomain

public enum ArchiveFormat: Equatable {
    case zip, sevenZip, tarGz

    public var fileExtension: String {
        switch self {
        case .zip: return ".zip"
        case .sevenZip: return ".7z"
        case .tarGz: return ".tar.gz"
        }
    }
}

public enum ArchiveError: Error, Equatable {
    case cancelled
    case failed(String)

    public var message: String {
        switch self {
        case .cancelled: return "Cancelled"
        case .failed(let text): return text
        }
    }
}

/// A scratch folder beside the sources or the archive. Work is done there, so a half-written result
/// never shows in the folder and cancelling can discard it whole.
final class Workspace {
    let path: String

    init(in parent: String) throws {
        path = (parent as NSString).appendingPathComponent(".compressing-\(getpid())-\(DispatchTime.now().uptimeNanoseconds)")
        try FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: false)
    }

    deinit { try? FileManager.default.removeItem(atPath: path) }
}

/// Extracting and compressing archives, in-process through libarchive.
public enum ArchiveTools {
    // MARK: names

    /// The folder an archive extracts into: its own name minus the archive suffixes, so
    /// `logs.tar.gz` becomes `logs` rather than `logs.tar`. A name that is only suffixes keeps itself.
    public static func extractionFolderName(archive: Location) -> String {
        let suffixes = [".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".zst", ".lzma", ".cpio", ".xar"]
        var stem = archive.displayName
        while true {
            let lower = stem.lowercased()
            guard let suffix = suffixes.first(where: { lower.hasSuffix($0) }), stem.count > suffix.count else { break }
            stem = String(stem.dropLast(suffix.count))
        }
        return stem
    }

    /// A child of `parent` that does not exist yet: `Trip`, then `Trip (2)`.
    public static func uniqueChild(of parent: String, base: String) -> String {
        var candidate = (parent as NSString).appendingPathComponent(base)
        var suffix = 2
        while PathInfo.exists(candidate) {
            candidate = (parent as NSString).appendingPathComponent("\(base) (\(suffix))")
            suffix += 1
        }
        return candidate
    }

    /// The archive's name before its extension: the item's own name for one item, a generic one for several.
    public static func defaultArchiveBase(names: [String]) -> String { names.count == 1 ? names[0] : "Archive" }

    /// A path in `parent` that does not exist yet: `Trip.zip`, then `Trip (2).zip`.
    public static func uniqueArchivePath(parent: String, base: String, format: ArchiveFormat) -> String {
        var candidate = (parent as NSString).appendingPathComponent(base + format.fileExtension)
        var suffix = 2
        while PathInfo.exists(candidate) {
            candidate = (parent as NSString).appendingPathComponent("\(base) (\(suffix))\(format.fileExtension)")
            suffix += 1
        }
        return candidate
    }

    // MARK: extraction

    /// Unpacks into a scratch folder beside the archive, then moves the result into place: an
    /// archive holding one folder lands as that folder; anything else gets a folder named after the
    /// archive. Blocks.
    public static func extractBlocking(archive: String, into parent: String, progress: @escaping (UInt64, UInt64) -> Void, cancelled: @escaping () -> Bool) throws {
        let workspace: Workspace
        do { workspace = try Workspace(in: parent) } catch { throw ArchiveError.failed("Could not write to this folder: \(error.localizedDescription)") }
        let unpacked = (workspace.path as NSString).appendingPathComponent("contents")
        try FileManager.default.createDirectory(atPath: unpacked, withIntermediateDirectories: true)
        let token = CancellationToken()
        let watcher = DispatchSource.makeTimerSource(queue: Queues.work)
        watcher.schedule(deadline: .now(), repeating: .milliseconds(20))
        watcher.setEventHandler { if cancelled() { token.cancel() } }
        watcher.resume()
        defer { watcher.cancel() }
        do { try ArchiveReader.extract(archive, to: unpacked, cancel: token, progress: progress) }
        catch is OperationCancelled { throw ArchiveError.cancelled }
        catch let failure as ArchiveReader.Failure { throw ArchiveError.failed("Could not extract the archive: \(failure.message)") }
        catch { throw ArchiveError.failed("Could not extract the archive: \(error.localizedDescription)") }
        if cancelled() { throw ArchiveError.cancelled }
        let names = ((try? FileManager.default.contentsOfDirectory(atPath: unpacked)) ?? []).sorted()
        guard !names.isEmpty else { throw ArchiveError.failed("The archive is empty") }
        let name: String
        let from: String
        if names.count == 1, PathInfo.at((unpacked as NSString).appendingPathComponent(names[0])) == .directory {
            // The archive is one folder: it lands beside the archive as it is, rather than inside
            // a second folder of the same name.
            name = names[0]
            from = (unpacked as NSString).appendingPathComponent(names[0])
        } else {
            name = extractionFolderName(archive: Location(localPath: archive))
            from = unpacked
        }
        let target = uniqueChild(of: parent, base: name)
        if rename(from, target) != 0 { throw ArchiveError.failed("Could not save the extracted files: \(String(cString: strerror(errno)))") }
    }

    /// Extracts a local archive on a worker and reports through a job on `manager`. Returns whether
    /// extraction began, which it does not for a location that is not a local archive.
    @discardableResult
    public static func extract(manager: OperationManager, archive: Location, parent: Location, onFailure: @escaping (String) -> Void) -> Bool {
        guard let archivePath = archive.localPath, let parentPath = parent.localPath else { return false }
        let job = manager.beginJob(.extract, sources: [archive], destination: parent)
        Queues.work.async {
            let result: Result<Void, ArchiveError>
            do {
                try extractBlocking(archive: archivePath, into: parentPath,
                                    progress: { done, total in job.setProgress(completed: done, total: total) }, cancelled: { job.isCancelled })
                result = .success(())
            } catch let error as ArchiveError { result = .failure(error) }
            catch { result = .failure(.failed(error.localizedDescription)) }
            switch result {
            case .success: job.finish(.success(()))
            case .failure(let error):
                job.finish(.failure(JobFailure(messageKey: "operation.failed")))
                if error != .cancelled { Main.deliver { onFailure(error.message) } }
            }
        }
        return true
    }

    // MARK: compression

    private struct Entry { var path: String; var name: String; var kind: Kind }
    private enum Kind { case directory, file, symlink(String) }

    /// Archives `sources`, which must all sit in one folder, into a new file there, and returns its
    /// path. `progress` receives (bytes read, bytes to read); a stop discards everything written.
    public static func compressBlocking(sources: [String], format: ArchiveFormat, progress: @escaping (UInt64, UInt64) -> Void, cancelled: @escaping () -> Bool) throws -> String {
        guard let first = sources.first else { throw ArchiveError.failed("Nothing is selected to compress.") }
        let parent = splitPath(first).parent
        var names: [String] = []
        for source in sources {
            if splitPath(source).parent != parent { throw ArchiveError.failed("Items from different folders cannot be compressed together.") }
            let name = splitPath(source).name
            if name.isEmpty { throw ArchiveError.failed("An item has no name to archive.") }
            names.append(name)
        }
        let base = defaultArchiveBase(names: names)
        let workspace: Workspace
        do { workspace = try Workspace(in: parent) } catch { throw ArchiveError.failed("Could not write to this folder: \(error.localizedDescription)") }
        let scratch = (workspace.path as NSString).appendingPathComponent(base + format.fileExtension)
        // Zip and tar keep links as links; 7-Zip has no portable link entry here and follows them.
        let entries = try collectEntries(sources, followSymlinks: format == .sevenZip, cancelled: cancelled)
        let total = entries.reduce(UInt64(0)) { sum, entry in
            guard case .file = entry.kind else { return sum }
            var status = stat()
            return stat(entry.path, &status) == 0 ? sum + UInt64(max(0, status.st_size)) : sum
        }
        if cancelled() { throw ArchiveError.cancelled }
        progress(0, total)
        try write(entries, to: scratch, format: format, total: total, progress: progress, cancelled: cancelled)
        if cancelled() { throw ArchiveError.cancelled }
        progress(total, total)
        let archive = uniqueArchivePath(parent: parent, base: base, format: format)
        if rename(scratch, archive) != 0 { throw ArchiveError.failed("Could not save the archive: \(String(cString: strerror(errno)))") }
        return archive
    }

    private static func collectEntries(_ sources: [String], followSymlinks: Bool, cancelled: () -> Bool) throws -> [Entry] {
        var entries: [Entry] = []
        var visited = Set<String>()
        func collect(_ path: String, _ name: String) throws {
            if cancelled() { throw ArchiveError.cancelled }
            var link = stat()
            guard lstat(path, &link) == 0 else { throw ArchiveError.failed("Could not read \(name): \(String(cString: strerror(errno)))") }
            if link.st_mode & S_IFMT == S_IFLNK && !followSymlinks {
                var buffer = [CChar](repeating: 0, count: Int(PATH_MAX))
                let length = readlink(path, &buffer, buffer.count - 1)
                entries.append(Entry(path: path, name: name, kind: .symlink(length > 0 ? String(cString: buffer) : "")))
                return
            }
            var status = stat()
            guard stat(path, &status) == 0 else { throw ArchiveError.failed("Could not read \(name): \(String(cString: strerror(errno)))") }
            if status.st_mode & S_IFMT == S_IFDIR {
                // A link that leads back into the walk would never end.
                if !visited.insert(realPath(path) ?? path).inserted { return }
                entries.append(Entry(path: path, name: name, kind: .directory))
                for child in try FileManager.default.contentsOfDirectory(atPath: path).sorted() {
                    try collect((path as NSString).appendingPathComponent(child), name + "/" + child)
                }
            } else {
                entries.append(Entry(path: path, name: name, kind: .file))
            }
        }
        for source in sources { try collect(source, splitPath(source).name) }
        return entries.sorted { $0.name < $1.name }
    }

    private static func write(_ entries: [Entry], to path: String, format: ArchiveFormat, total: UInt64, progress: (UInt64, UInt64) -> Void, cancelled: () -> Bool) throws {
        guard let writer = archive_write_new() else { throw ArchiveError.failed("Could not create the archive") }
        defer { _ = archive_write_free(writer) }
        switch format {
        case .zip: _ = archive_write_set_format_zip(writer)
        case .sevenZip: _ = archive_write_set_format_7zip(writer)
        case .tarGz:
            _ = archive_write_add_filter_gzip(writer)
            _ = archive_write_set_format_pax_restricted(writer)
        }
        guard archive_write_open_filename(writer, path) == ARCHIVE_OK else {
            throw ArchiveError.failed("Could not create the archive: \(archive_error_string(writer).map { String(cString: $0) } ?? "")")
        }
        var done: UInt64 = 0
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        for entry in entries {
            if cancelled() { throw ArchiveError.cancelled }
            var status = stat()
            guard stat(entry.path, &status) == 0 || lstat(entry.path, &status) == 0 else { throw ArchiveError.failed("Could not read \(entry.name)") }
            guard let header = archive_entry_new() else { throw ArchiveError.failed("Could not create the archive") }
            defer { archive_entry_free(header) }
            archive_entry_set_pathname(header, entry.name)
            archive_entry_set_perm(header, status.st_mode & 0o777)
            archive_entry_set_mtime(header, status.st_mtimespec.tv_sec, 0)
            switch entry.kind {
            case .directory:
                archive_entry_set_filetype(header, UInt32(S_IFDIR))
                archive_entry_set_size(header, 0)
            case .symlink(let target):
                archive_entry_set_filetype(header, UInt32(S_IFLNK))
                archive_entry_set_symlink(header, target)
                archive_entry_set_size(header, 0)
            case .file:
                archive_entry_set_filetype(header, UInt32(S_IFREG))
                archive_entry_set_size(header, Int64(status.st_size))
            }
            guard archive_write_header(writer, header) == ARCHIVE_OK else {
                throw ArchiveError.failed("Could not write \(entry.name): \(archive_error_string(writer).map { String(cString: $0) } ?? "")")
            }
            guard case .file = entry.kind else { continue }
            let fd = Darwin.open(entry.path, O_RDONLY)
            if fd < 0 { throw ArchiveError.failed("Could not read \(entry.name): \(String(cString: strerror(errno)))") }
            defer { close(fd) }
            while true {
                if cancelled() { throw ArchiveError.cancelled }
                let read = Darwin.read(fd, &buffer, buffer.count)
                if read < 0 { throw ArchiveError.failed("Could not read \(entry.name): \(String(cString: strerror(errno)))") }
                if read == 0 { break }
                if archive_write_data(writer, buffer, read) < 0 {
                    throw ArchiveError.failed("Could not write \(entry.name): \(archive_error_string(writer).map { String(cString: $0) } ?? "")")
                }
                done += UInt64(read)
                progress(min(done, total), total)
            }
        }
        guard archive_write_close(writer) == ARCHIVE_OK else { throw ArchiveError.failed("Could not finish the archive") }
    }

    /// Compresses on a worker, reporting through a job on `manager`, and hands the result to `onDone`.
    public static func compress(manager: OperationManager, sources: [Location], format: ArchiveFormat, onDone: @escaping (Result<String, ArchiveError>) -> Void) {
        let paths = sources.compactMap(\.localPath)
        let job = manager.beginJob(.compress, sources: sources, destination: nil)
        Queues.work.async {
            let result: Result<String, ArchiveError>
            do {
                result = .success(try compressBlocking(sources: paths, format: format,
                                                       progress: { done, total in job.setProgress(completed: done, total: total) }, cancelled: { job.isCancelled }))
            } catch let error as ArchiveError { result = .failure(error) }
            catch { result = .failure(.failed(error.localizedDescription)) }
            switch result {
            case .success: job.finish(.success(()))
            case .failure: job.finish(.failure(JobFailure(messageKey: "operation.failed")))
            }
            Main.deliver { onDone(result) }
        }
    }
}
