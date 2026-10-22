import Foundation

/// A place a sidebar row navigates to, with its label and symbol.
public struct SidebarLocation: Equatable {
    public var location: Location
    public var label: String
    /// An SF Symbol name.
    public var iconName: String

    public init(location: Location, label: String, iconName: String) {
        self.location = location
        self.label = label
        self.iconName = iconName
    }

    public var uri: String { location.uri }
    public var localPath: String? { location.localPath }
}

/// One row. Children are directories loaded on demand when the row is expanded.
public final class SidebarNode {
    public let location: SidebarLocation
    public let isExpandable: Bool
    public var children: [SidebarNode] = []

    public init(location: SidebarLocation, isExpandable: Bool = true) {
        self.location = location
        self.isExpandable = isExpandable
    }

    convenience init(path: String, label: String, icon: String, expandable: Bool = true) {
        self.init(location: SidebarLocation(location: Location(localPath: path), label: label, iconName: icon), isExpandable: expandable)
    }
}

/// A volume that is attached but has nothing mounted. Activating it has to mount it first.
public struct MountableVolume: Equatable {
    /// Identity used to find the volume again when the row is activated.
    public var id: String
    public var label: String
    public var iconName: String

    public init(id: String, label: String, iconName: String) {
        self.id = id
        self.label = label
        self.iconName = iconName
    }
}

/// A mounted volume as the system reports it.
public struct MountedVolume: Equatable {
    public enum Kind: Int { case removable = 0, internalDisk = 1, other = 2 }
    public var name: String
    public var path: String
    public var kind: Kind
    public var isEjectable: Bool

    public init(name: String, path: String, kind: Kind, isEjectable: Bool = false) {
        self.name = name
        self.path = path
        self.kind = kind
        self.isEjectable = isEjectable
    }
}

/// Where the sidebar learns about volumes. The macOS app supplies DiskArbitration and NSWorkspace.
public protocol VolumeProvider: AnyObject {
    func mountedVolumes() -> [MountedVolume]
    func unmountedVolumes() -> [MountableVolume]
}

public final class NoVolumes: VolumeProvider {
    public init() {}
    public func mountedVolumes() -> [MountedVolume] { [] }
    public func unmountedVolumes() -> [MountableVolume] { [] }
}

public struct SidebarSection {
    public var title: String
    public var nodes: [SidebarNode]
    public var mountables: [MountableVolume]

    public init(title: String, nodes: [SidebarNode] = [], mountables: [MountableVolume] = []) {
        self.title = title
        self.nodes = nodes
        self.mountables = mountables
    }
}

/// Pinned locations and which parts of the sidebar are expanded, persisted as canonical URIs so a
/// location that is missing now keeps the identity it was pinned with.
public final class SidebarPreferences {
    public static let quickAccess = "Quick access"
    public static let devices = "Devices"

    private let file: URL?
    public private(set) var pins: [Location] = []
    private var sectionExpanded: [String: Bool] = [:]
    private var expandedOccurrences: [String: Bool] = [:]
    public private(set) var defaultExpandedSection = SidebarPreferences.quickAccess
    private var needsDefaults: Bool
    public private(set) var notices: [RecoverableStateNotice] = []

    public static func inMemory() -> SidebarPreferences { SidebarPreferences(file: nil) }

    public init(file: URL?) {
        self.file = file
        needsDefaults = file != nil
        guard let file, FileManager.default.fileExists(atPath: file.path) else { return }
        needsDefaults = false
        guard let data = try? BoundedState.read(file), let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            notices.append(.malformedRecord)
            return
        }
        for value in (root["locations"] as? [Any]) ?? [] {
            if let uri = value as? String, let location = try? Location(uri: uri) { pin(location) } else { notices.append(.malformedRecord) }
        }
        sectionExpanded = (root["section_expanded"] as? [String: Bool]) ?? [:]
        expandedOccurrences = (root["expanded_occurrences"] as? [String: Bool]) ?? [:]
        defaultExpandedSection = (root["default_expanded_section"] as? String) ?? SidebarPreferences.quickAccess
    }

    public func pin(_ location: Location) { if !pins.contains(location) { pins.append(location) } }
    public func unpin(_ location: Location) { pins.removeAll { $0 == location } }
    public func isPinned(_ location: Location) -> Bool { pins.contains(location) }

    public func isSectionExpanded(_ section: String) -> Bool { sectionExpanded[section] ?? (section == defaultExpandedSection || section != SidebarPreferences.devices) }
    public func setSectionExpanded(_ section: String, _ expanded: Bool) { sectionExpanded[section] = expanded }

    /// The devices group is collapsed until the user opens it.
    public var devicesExpanded: Bool {
        get { sectionExpanded[SidebarPreferences.devices] ?? false }
        set { sectionExpanded[SidebarPreferences.devices] = newValue }
    }

    private func key(_ occurrence: [String]) -> String { occurrence.joined(separator: "\u{1F}") }
    public func isOccurrenceExpanded(_ occurrence: [String]) -> Bool { expandedOccurrences[key(occurrence)] ?? false }
    public func setOccurrenceExpanded(_ occurrence: [String], _ expanded: Bool) {
        if expanded { expandedOccurrences[key(occurrence)] = true } else { expandedOccurrences.removeValue(forKey: key(occurrence)) }
    }

    public func save() throws {
        guard let file else { return }
        let object: [String: Any] = ["schema_version": 2, "locations": pins.map(\.uri), "section_expanded": sectionExpanded,
                                     "expanded_occurrences": expandedOccurrences, "default_expanded_section": defaultExpandedSection]
        try BoundedState.write(try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]), to: file)
    }

    /// The first run pins the folders people reach for first.
    func installDefaultPins(home: String, exists: (String) -> Bool = { FileManager.default.fileExists(atPath: $0) }) {
        guard needsDefaults else { return }
        needsDefaults = false
        for folder in [home, home + "/Desktop", home + "/Documents", home + "/Downloads", home + "/Pictures"] where exists(folder) {
            pin(Location(localPath: folder))
        }
    }
}

/// Builds the sidebar's sections: Quick access (pins), the user's folders, and This Mac.
public final class SidebarModel {
    public let preferences: SidebarPreferences
    private let home: String
    private let userName: String
    private let volumes: VolumeProvider
    private let exists: (String) -> Bool

    public init(preferences: SidebarPreferences, home: String = NSHomeDirectory(), userName: String = NSFullUserName(),
                volumes: VolumeProvider = NoVolumes(), exists: @escaping (String) -> Bool = { FileManager.default.fileExists(atPath: $0) }) {
        self.preferences = preferences
        self.home = home
        self.userName = userName
        self.volumes = volumes
        self.exists = exists
        preferences.installDefaultPins(home: home, exists: exists)
    }

    public func sections() -> [SidebarSection] { [quickAccess(), userFolders(), thisMac()] }

    static func standardFolderIcon(_ path: String) -> String {
        switch (path as NSString).lastPathComponent {
        case "Desktop": return "menubar.dock.rectangle"
        case "Documents": return "doc"
        case "Downloads": return "arrow.down.circle"
        case "Music": return "music.note"
        case "Pictures": return "photo"
        case "Movies": return "film"
        case "Applications": return "app"
        default: return "folder"
        }
    }

    private static func displayName(_ path: String) -> String {
        let name = (path as NSString).lastPathComponent
        return name.isEmpty ? path : name
    }

    private func quickAccess() -> SidebarSection {
        let nodes = preferences.pins.map { pin -> SidebarNode in
            let path = pin.localPath
            return SidebarNode(location: SidebarLocation(location: pin, label: path.map(SidebarModel.displayName) ?? pin.displayName,
                                                         iconName: path.map(SidebarModel.standardFolderIcon) ?? "folder"))
        }
        return SidebarSection(title: SidebarPreferences.quickAccess, nodes: nodes)
    }

    /// The section is titled with the account's own name, falling back to the home folder's.
    private func userSectionTitle() -> String {
        let trimmed = userName.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty || trimmed.lowercased() == "unknown" ? SidebarModel.displayName(home) : trimmed
    }

    private func userFolders() -> SidebarSection {
        var nodes = [SidebarNode(path: home, label: "Home", icon: "house")]
        for name in ["Desktop", "Documents", "Downloads", "Movies", "Music", "Pictures", "Applications"] {
            let path = home + "/" + name
            if exists(path) { nodes.append(SidebarNode(path: path, label: name, icon: SidebarModel.standardFolderIcon(path))) }
        }
        return SidebarSection(title: userSectionTitle(), nodes: nodes)
    }

    private func thisMac() -> SidebarSection {
        var nodes = [SidebarNode(path: "/", label: "Macintosh HD", icon: "internaldrive")]
        let mounted = volumes.mountedVolumes()
            .filter { $0.path != "/" && !$0.path.hasPrefix(home) }
            .sorted { ($0.kind.rawValue, $0.name.lowercased()) < ($1.kind.rawValue, $1.name.lowercased()) }
        for volume in mounted {
            nodes.append(SidebarNode(path: volume.path, label: volume.name, icon: volume.kind == .removable ? "externaldrive" : "internaldrive"))
        }
        let mountable = volumes.unmountedVolumes().sorted { $0.label.lowercased() < $1.label.lowercased() }
        return SidebarSection(title: "This Mac", nodes: nodes, mountables: mountable)
    }

    /// Converts an owned directory snapshot into expandable tree nodes (folders only), sorted by label.
    public static func children(from snapshot: DirectorySnapshot) -> [SidebarNode] {
        snapshot.entries.filter { $0.kind == .directory }
            .map { SidebarNode(location: SidebarLocation(location: $0.location, label: $0.displayName, iconName: "folder"), isExpandable: true) }
            .sorted { $0.location.label < $1.location.label }
    }
}
