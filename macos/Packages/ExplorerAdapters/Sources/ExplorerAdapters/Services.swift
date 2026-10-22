import AppKit
import DiskArbitration
import Foundation
import QuickLookThumbnailing
import UniformTypeIdentifiers
import ExplorerDomain

/// Volumes as the sidebar needs them: the mounted ones from the system, and attached devices that
/// have nothing mounted (found through DiskArbitration) so a stick that was ejected but left
/// plugged in can still be mounted from the sidebar.
public final class SystemVolumes: VolumeProvider {
    private var session: DASession?
    private var unmounted: [String: MountableVolume] = [:]
    private var observers: [NSObjectProtocol] = []
    /// Called on the main thread when the set of volumes may have changed.
    public var onChange: (() -> Void)?

    public init() {
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didMountNotification, NSWorkspace.didUnmountNotification, NSWorkspace.didRenameVolumeNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in self?.onChange?() })
        }
        startDiskArbitration()
    }

    private final class WeakVolumes { weak var target: SystemVolumes?; init(_ target: SystemVolumes) { self.target = target } }

    deinit {
        observers.forEach { NSWorkspace.shared.notificationCenter.removeObserver($0) }
        if let session { DASessionSetDispatchQueue(session, nil) }
    }

    public func mountedVolumes() -> [MountedVolume] {
        let keys: [URLResourceKey] = [.volumeNameKey, .volumeIsInternalKey, .volumeIsRemovableKey, .volumeIsEjectableKey, .volumeIsLocalKey, .volumeIsBrowsableKey]
        let urls = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: keys, options: [.skipHiddenVolumes]) ?? []
        return urls.compactMap { url in
            guard let values = try? url.resourceValues(forKeys: Set(keys)), values.volumeIsLocal == true, values.volumeIsBrowsable != false else { return nil }
            let removable = values.volumeIsRemovable == true || values.volumeIsEjectable == true || values.volumeIsInternal == false
            return MountedVolume(name: values.volumeName ?? url.lastPathComponent, path: url.path, kind: removable ? .removable : .internalDisk,
                                 isEjectable: values.volumeIsEjectable == true || values.volumeIsRemovable == true)
        }
    }

    public func unmountedVolumes() -> [MountableVolume] { unmounted.values.sorted { $0.label < $1.label } }

    private func startDiskArbitration() {
        guard let session = DASessionCreate(kCFAllocatorDefault) else { return }
        self.session = session
        // Callbacks already queued on the main queue can run after this object is gone, so they get a
        // weak reference through a box that is deliberately never freed (a few bytes per instance).
        let context = Unmanaged.passRetained(WeakVolumes(self)).toOpaque()
        DARegisterDiskAppearedCallback(session, nil, { disk, context in
            guard let context else { return }
            Unmanaged<WeakVolumes>.fromOpaque(context).takeUnretainedValue().target?.diskChanged(disk)
        }, context)
        DARegisterDiskDescriptionChangedCallback(session, nil, nil, { disk, _, context in
            guard let context else { return }
            Unmanaged<WeakVolumes>.fromOpaque(context).takeUnretainedValue().target?.diskChanged(disk)
        }, context)
        DARegisterDiskDisappearedCallback(session, nil, { disk, context in
            guard let context else { return }
            Unmanaged<WeakVolumes>.fromOpaque(context).takeUnretainedValue().target?.diskGone(disk)
        }, context)
        DASessionSetDispatchQueue(session, DispatchQueue.main)
    }

    private func diskChanged(_ disk: DADisk) {
        guard let description = DADiskCopyDescription(disk) as? [String: Any], let id = description[kDADiskDescriptionBusPathKey as String] as? String ?? (description[kDADiskDescriptionMediaBSDNameKey as String] as? String) else { return }
        let mountable = (description[kDADiskDescriptionVolumeMountableKey as String] as? Bool) == true
        let mounted = description[kDADiskDescriptionVolumePathKey as String] != nil
        let network = (description[kDADiskDescriptionVolumeNetworkKey as String] as? Bool) == true
        let internalDisk = (description[kDADiskDescriptionDeviceInternalKey as String] as? Bool) == true
        let name = (description[kDADiskDescriptionVolumeNameKey as String] as? String) ?? (description[kDADiskDescriptionMediaNameKey as String] as? String) ?? "Disk"
        // Network volumes appear elsewhere (or not at all); a mounted volume is not "mountable".
        if mountable && !mounted && !network && !internalDisk {
            unmounted[id] = MountableVolume(id: (description[kDADiskDescriptionMediaBSDNameKey as String] as? String) ?? id, label: name, iconName: "externaldrive")
        } else {
            unmounted[id] = nil
        }
        onChange?()
    }

    private func diskGone(_ disk: DADisk) {
        guard let description = DADiskCopyDescription(disk) as? [String: Any] else { return }
        let id = (description[kDADiskDescriptionBusPathKey as String] as? String) ?? (description[kDADiskDescriptionMediaBSDNameKey as String] as? String) ?? ""
        unmounted[id] = nil
        onChange?()
    }

    /// Mounts an attached device by its BSD name (`MountableVolume.id`).
    public func mount(id: String, completion: @escaping (Bool) -> Void) {
        guard let session, let disk = DADiskCreateFromBSDName(kCFAllocatorDefault, session, id) else { return completion(false) }
        final class Box { let done: (Bool) -> Void; init(_ done: @escaping (Bool) -> Void) { self.done = done } }
        let box = Unmanaged.passRetained(Box(completion))
        DADiskMount(disk, nil, DADiskMountOptions(kDADiskMountOptionDefault), { _, dissenter, context in
            guard let context else { return }
            let box = Unmanaged<Box>.fromOpaque(context).takeRetainedValue()
            DispatchQueue.main.async { box.done(dissenter == nil) }
        }, box.toOpaque())
    }

    /// Unmounts and ejects a volume. The system asks the user about anything still using it.
    public static func eject(path: String, completion: @escaping (Error?) -> Void) {
        do {
            try NSWorkspace.shared.unmountAndEjectDevice(at: URL(fileURLWithPath: path))
            completion(nil)
        } catch { completion(error) }
    }
}

/// Opening things: the default application, a chosen one, and Terminal.
public enum OpenService {
    public static func open(_ location: Location) -> Bool {
        guard let path = location.localPath else { return false }
        return NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    /// Applications that can open this item, the default first.
    public static func applications(toOpen location: Location) -> [(name: String, url: URL, isDefault: Bool)] {
        guard let path = location.localPath else { return [] }
        let file = URL(fileURLWithPath: path)
        let defaultApp = NSWorkspace.shared.urlForApplication(toOpen: file)
        return NSWorkspace.shared.urlsForApplications(toOpen: file).map { url in
            (FileManager.default.displayName(atPath: url.path).replacingOccurrences(of: ".app", with: ""), url, url == defaultApp)
        }.sorted { ($0.isDefault ? 0 : 1, $0.name.lowercased()) < ($1.isDefault ? 0 : 1, $1.name.lowercased()) }
    }

    public static func open(_ locations: [Location], with application: URL, completion: @escaping (Error?) -> Void = { _ in }) {
        let urls = locations.compactMap { $0.localPath.map { URL(fileURLWithPath: $0) } }
        NSWorkspace.shared.open(urls, withApplicationAt: application, configuration: NSWorkspace.OpenConfiguration()) { _, error in
            DispatchQueue.main.async { completion(error) }
        }
    }

    /// Opens Terminal at a local folder.
    public static func openTerminal(at location: Location) -> Bool {
        guard let path = location.localPath, let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else { return false }
        NSWorkspace.shared.open([URL(fileURLWithPath: path)], withApplicationAt: terminal, configuration: NSWorkspace.OpenConfiguration())
        return true
    }

    /// The localized description of a content type, for the Type column and group headings.
    public static func typeDescription(forContentType type: String, isDirectory: Bool) -> String {
        if isDirectory { return "Folder" }
        return UTType(type)?.localizedDescription ?? UTType(mimeType: type)?.localizedDescription ?? "Document"
    }
}

/// Thumbnails for visible rows only, cancelled when the view moves on.
public final class ThumbnailService {
    public init() {}

    /// A handle that cancels the request.
    public final class Request {
        fileprivate let request: QLThumbnailGenerator.Request
        fileprivate init(_ request: QLThumbnailGenerator.Request) { self.request = request }
        public func cancel() { QLThumbnailGenerator.shared.cancel(request) }
    }

    @discardableResult
    public func thumbnail(for location: Location, size: CGSize, scale: CGFloat, completion: @escaping (NSImage?) -> Void) -> Request? {
        guard let path = location.localPath else { completion(nil); return nil }
        let request = QLThumbnailGenerator.Request(fileAt: URL(fileURLWithPath: path), size: size, scale: scale, representationTypes: .all)
        QLThumbnailGenerator.shared.generateBestRepresentation(for: request) { representation, _ in
            DispatchQueue.main.async { completion(representation?.nsImage) }
        }
        return Request(request)
    }
}

/// The values the properties window shows for one local item.
public enum PropertyLoader {
    public static func load(_ path: String) throws -> PropertyDetails {
        var status = stat()
        guard stat(path, &status) == 0 else { throw POSIXFailure() }
        func name(_ id: UInt32, group: Bool) -> String {
            if group, let entry = getgrgid(id) { return String(cString: entry.pointee.gr_name) }
            if !group, let entry = getpwuid(id) { return String(cString: entry.pointee.pw_name) }
            return String(id)
        }
        func date(_ spec: timespec) -> Date { Date(timeIntervalSince1970: TimeInterval(spec.tv_sec)) }
        let isDirectory = status.st_mode & S_IFMT == S_IFDIR
        return PropertyDetails(
            path: path, name: path == "/" ? "/" : (path as NSString).lastPathComponent, isDirectory: isDirectory,
            contentType: ContentTypes.contentType(forName: (path as NSString).lastPathComponent, isDirectory: isDirectory),
            size: UInt64(max(0, status.st_size)), created: date(status.st_birthtimespec), modified: date(status.st_mtimespec), accessed: date(status.st_atimespec),
            owner: name(status.st_uid, group: false), group: name(status.st_gid, group: true), permissions: PermissionBits(mode: UInt16(status.st_mode & 0o777)))
    }

    /// Applies new permission bits. Ownership is not changed from here.
    public static func setPermissions(_ bits: PermissionBits, at path: String) throws {
        if chmod(path, mode_t(bits.mode)) != 0 { throw POSIXFailure() }
    }
}
