import Darwin
import Foundation
import ExplorerDomain

// Shared plumbing for the adapters. Ports are main-thread: an adapter does its work on a
// background queue and hops back to the main thread to answer, and it always hops, even when the
// answer is known at once, so a callback never runs before the call that asked for it returns.

public enum Main {
    /// Delivers on the main thread, after the current call has returned.
    public static func deliver(_ block: @escaping () -> Void) { DispatchQueue.main.async(execute: block) }
}

enum Queues {
    /// Filesystem work. Concurrent, so one slow transfer never stalls a listing or a search.
    static let work = DispatchQueue(label: "casa.alera.explorer.work", qos: .userInitiated, attributes: .concurrent)
}

/// A POSIX failure, carried until it is turned into a port error.
struct POSIXFailure: Error {
    let code: Int32
    init(_ code: Int32 = errno) { self.code = code }

    /// The platform-neutral failure this errno stands for (`spec/contracts/errors.json`).
    var failure: BackendFailure {
        switch code {
        case EACCES, EPERM: return .permissionDenied
        case ENOENT: return .notFound
        case EEXIST: return .exists
        case ENOTEMPTY: return .wouldMerge
        case EROFS: return .readOnly
        case ENOTSUP, EOPNOTSUPP: return .notSupported
        case EINVAL, EISDIR: return .invalidArgument
        case ENAMETOOLONG: return .invalidFilename
        case ENXIO, ENODEV, ENOTCONN: return .notConnected
        default: return .other
        }
    }
}

struct OperationCancelled: Error {}

/// Maps whatever a Foundation call threw onto a failure, looking through `NSError` wrappers.
func backendFailure(_ error: Error) -> BackendFailure {
    if let posix = error as? POSIXFailure { return posix.failure }
    let ns = error as NSError
    if ns.domain == NSPOSIXErrorDomain { return POSIXFailure(Int32(ns.code)).failure }
    if let underlying = ns.userInfo[NSUnderlyingErrorKey] as? NSError, underlying.domain == NSPOSIXErrorDomain {
        return POSIXFailure(Int32(underlying.code)).failure
    }
    if ns.domain == NSCocoaErrorDomain {
        switch ns.code {
        case NSFileNoSuchFileError, NSFileReadNoSuchFileError: return .notFound
        case NSFileReadNoPermissionError, NSFileWriteNoPermissionError: return .permissionDenied
        case NSFileWriteFileExistsError: return .exists
        case NSFileWriteVolumeReadOnlyError: return .readOnly
        case NSFileReadInvalidFileNameError, NSFileWriteInvalidFileNameError: return .invalidFilename
        default: return .other
        }
    }
    return .other
}

public enum PathInfo {
    case missing
    case file
    case directory
    case symlink
    case other

    /// What lives at `path`, without following a symbolic link.
    public static func at(_ path: String) -> PathInfo {
        var status = stat()
        if lstat(path, &status) != 0 { return .missing }
        switch status.st_mode & S_IFMT {
        case S_IFDIR: return .directory
        case S_IFREG: return .file
        case S_IFLNK: return .symlink
        default: return .other
        }
    }

    public static func exists(_ path: String) -> Bool { at(path) != .missing }
}

extension Location {
    /// The local path, or nil for a remote or archive location.
    var filePath: String? { localPath }
}

/// The directory part of a path and its last component.
func splitPath(_ path: String) -> (parent: String, name: String) {
    let url = URL(fileURLWithPath: path)
    return (url.deletingLastPathComponent().path, url.lastPathComponent)
}

/// The path with every symbolic link resolved (`/var` is `/private/var`).
func realPath(_ path: String) -> String? {
    guard let resolved = realpath(path, nil) else { return nil }
    defer { free(resolved) }
    return String(cString: resolved)
}
