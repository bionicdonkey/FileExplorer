import CArchive
import Darwin
import Foundation
import ExplorerDomain

/// Reading archives with libarchive.
///
/// An archive is opened from its own file, which libarchive can seek, so a 7-Zip index that is not
/// at the front still lists. Listing reads only entry headers; file data is not decompressed.
enum ArchiveReader {
    struct FileMeta { var size: UInt64; var modified: Date? }

    /// One archive's entries as relative, decoded paths.
    struct Index {
        var dirs = Set<String>()
        var files: [String: FileMeta] = [:]

        struct Child { var name: String; var isDirectory: Bool; var size: UInt64?; var modified: Date? }

        /// Directories and files together, by name (case-insensitively, then exactly).
        func children(of parent: String) -> [Child] {
            var children: [Child] = []
            for dir in dirs where ArchiveReader.parent(of: dir) == parent {
                children.append(Child(name: ArchiveReader.name(of: dir), isDirectory: true, size: nil, modified: nil))
            }
            for (path, meta) in files where ArchiveReader.parent(of: path) == parent {
                children.append(Child(name: ArchiveReader.name(of: path), isDirectory: false, size: meta.size, modified: meta.modified))
            }
            return children.sorted {
                let (l, r) = ($0.name.lowercased(), $1.name.lowercased())
                return l == r ? $0.name < $1.name : l < r
            }
        }
    }

    static func parent(of path: String) -> String {
        guard let slash = path.lastIndex(of: "/") else { return "" }
        return String(path[..<slash])
    }

    static func name(of path: String) -> String {
        guard let slash = path.lastIndex(of: "/") else { return path }
        return String(path[path.index(after: slash)...])
    }

    /// A path inside the archive as safe components, rejoined: nil for an entry that steps out of
    /// the archive with `..` or is empty. A leading `./` and empty components are dropped.
    static func safeRelativePath(_ name: String) -> String? {
        var parts: [Substring] = []
        for part in name.split(separator: "/", omittingEmptySubsequences: false) {
            switch part {
            case "", ".": continue
            case "..": return nil
            default: parts.append(part)
            }
        }
        return parts.isEmpty ? nil : parts.joined(separator: "/")
    }

    struct Failure: Error { var message: String }

    static func open(_ path: String) throws -> OpaquePointer {
        guard let reader = archive_read_new() else { throw Failure(message: "Could not read the archive") }
        _ = archive_read_support_format_all(reader)
        _ = archive_read_support_filter_all(reader)
        guard archive_read_open_filename(reader, path, 65536) == ARCHIVE_OK else {
            let message = archive_error_string(reader).map { String(cString: $0) } ?? "Could not open the archive"
            _ = archive_read_free(reader)
            throw Failure(message: message)
        }
        return reader
    }

    /// Reads every entry header, recording directories (including ones the archive never stored
    /// explicitly) and files with their size and modification time.
    static func readIndex(_ path: String, cancel: CancellationToken? = nil) throws -> Index {
        let reader = try open(path)
        defer { _ = archive_read_free(reader) }
        var index = Index()
        var entry: OpaquePointer?
        while true {
            if cancel?.isCancelled == true { throw OperationCancelled() }
            let status = archive_read_next_header(reader, &entry)
            if status == ARCHIVE_EOF { break }
            guard status == ARCHIVE_OK || status == ARCHIVE_WARN, let entry else {
                throw Failure(message: archive_error_string(reader).map { String(cString: $0) } ?? "Could not read the archive")
            }
            guard let raw = archive_entry_pathname(entry), let relative = safeRelativePath(String(cString: raw)) else {
                _ = archive_read_data_skip(reader)
                continue
            }
            var ancestor = parent(of: relative)
            while !ancestor.isEmpty { index.dirs.insert(ancestor); ancestor = parent(of: ancestor) }
            if archive_entry_filetype(entry) == UInt32(S_IFDIR) {
                index.dirs.insert(relative)
            } else {
                let seconds = archive_entry_mtime(entry)
                index.files[relative] = FileMeta(size: UInt64(max(0, archive_entry_size(entry))), modified: seconds >= 0 ? Date(timeIntervalSince1970: TimeInterval(seconds)) : nil)
            }
            _ = archive_read_data_skip(reader)
        }
        return index
    }

    /// Unpacks into `destination`, entry by entry, reporting bytes of the archive file consumed
    /// and stopping promptly on cancellation. `include` filters by relative path.
    static func extract(_ path: String, to destination: String, include: (String) -> Bool = { _ in true },
                        cancel: CancellationToken, progress: (UInt64, UInt64) -> Void) throws {
        let total = UInt64((try? FileManager.default.attributesOfItem(atPath: path)[.size] as? UInt64) ?? 0)
        let reader = try open(path)
        defer { _ = archive_read_free(reader) }
        progress(0, total)
        var entry: OpaquePointer?
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            if cancel.isCancelled { throw OperationCancelled() }
            let status = archive_read_next_header(reader, &entry)
            if status == ARCHIVE_EOF { break }
            guard status == ARCHIVE_OK || status == ARCHIVE_WARN, let entry else {
                throw Failure(message: archive_error_string(reader).map { String(cString: $0) } ?? "Could not extract the archive")
            }
            guard let raw = archive_entry_pathname(entry), let relative = safeRelativePath(String(cString: raw)), include(relative) else {
                _ = archive_read_data_skip(reader)
                continue
            }
            let target = (destination as NSString).appendingPathComponent(relative)
            let type = archive_entry_filetype(entry)
            let mode = archive_entry_perm(entry) & 0o777
            if type == UInt32(S_IFDIR) {
                try FileManager.default.createDirectory(atPath: target, withIntermediateDirectories: true)
                chmod(target, mode | 0o700)
            } else if type == UInt32(S_IFLNK), let link = archive_entry_symlink(entry) {
                try FileManager.default.createDirectory(atPath: (target as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
                if symlink(link, target) != 0 && errno != EEXIST { throw POSIXFailure() }
            } else if type == UInt32(S_IFREG) {
                try FileManager.default.createDirectory(atPath: (target as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
                let fd = Darwin.open(target, O_WRONLY | O_CREAT | O_TRUNC, 0o600)
                if fd < 0 { throw POSIXFailure() }
                defer { close(fd) }
                while true {
                    if cancel.isCancelled { throw OperationCancelled() }
                    let read = archive_read_data(reader, &buffer, buffer.count)
                    if read == 0 { break }
                    if read < 0 { throw Failure(message: archive_error_string(reader).map { String(cString: $0) } ?? "Could not extract the archive") }
                    var written = 0
                    while written < read {
                        let n = buffer.withUnsafeBytes { write(fd, $0.baseAddress! + written, read - written) }
                        if n < 0 { throw POSIXFailure() }
                        written += n
                    }
                    progress(min(UInt64(max(0, archive_filter_bytes(reader, -1))), total), total)
                }
                fchmod(fd, mode)
                let seconds = archive_entry_mtime(entry)
                if seconds > 0 {
                    var times = [timespec(tv_sec: seconds, tv_nsec: 0), timespec(tv_sec: seconds, tv_nsec: 0)]
                    futimens(fd, &times)
                }
            } else {
                _ = archive_read_data_skip(reader)
            }
            progress(min(UInt64(max(0, archive_filter_bytes(reader, -1))), total), total)
        }
        progress(total, total)
    }
}
