import CoreServices
import Foundation
import ExplorerDomain

/// Watches one directory with FSEvents and says only that something under it changed. Events for
/// anything deeper than a direct child are ignored, and FSEvents' own coalescing means a burst of
/// changes can arrive as one pulse. This is coarser than a per-file monitor, which is why watching
/// is `approximate` on macOS.
enum DirectoryWatcher {
    private final class Box { let changed: () -> Void; let directory: String
        init(directory: String, changed: @escaping () -> Void) { self.directory = directory; self.changed = changed } }

    private final class Handle: WatchHandle {
        var stream: FSEventStreamRef?
        var box: Unmanaged<Box>?
        func cancel() {
            if let stream { FSEventStreamStop(stream); FSEventStreamInvalidate(stream); FSEventStreamRelease(stream) }
            stream = nil
            box?.release()
            box = nil
        }
        deinit { cancel() }
    }

    static func watch(_ location: Location, changed: @escaping () -> Void) -> WatchHandle? {
        guard let path = location.localPath else { return nil }
        let box = Unmanaged.passRetained(Box(directory: realPath(path) ?? path, changed: changed))
        var context = FSEventStreamContext(version: 0, info: box.toOpaque(), retain: nil, release: nil, copyDescription: nil)
        let callback: FSEventStreamCallback = { _, info, count, paths, flags, _ in
            guard let info else { return }
            let box = Unmanaged<Box>.fromOpaque(info).takeUnretainedValue()
            // With UseCFTypes the paths arrive as a CFArray of CFString, not a C string array.
            let list = Unmanaged<NSArray>.fromOpaque(paths).takeUnretainedValue() as? [String] ?? []
            var affected = false
            for index in 0..<count {
                let flag = flags[index]
                // Lost events or a rescan request mean "something changed, look again".
                if flag & UInt32(kFSEventStreamEventFlagMustScanSubDirs) != 0 { affected = true; break }
                let path = (list[index] as NSString).deletingLastPathComponent
                let directPath = list[index] == box.directory ? list[index] : path
                if directPath == box.directory { affected = true; break }
            }
            if affected { DispatchQueue.main.async { box.changed() } }
        }
        guard let stream = FSEventStreamCreate(nil, callback, &context, [box.takeUnretainedValue().directory] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                                               0.1, FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagUseCFTypes)) else {
            box.release()
            return nil
        }
        let handle = Handle()
        handle.stream = stream
        handle.box = box
        FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
        FSEventStreamStart(stream)
        return handle
    }
}
