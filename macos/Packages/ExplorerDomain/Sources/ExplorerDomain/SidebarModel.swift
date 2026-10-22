import Foundation

/// A titled group of sidebar rows. The full sidebar model (quick access, pins, devices) builds on this.
public struct SidebarSection {
    public var title: String
    public var nodes: [SidebarNode]
    public var mountables: [SidebarNode]

    public init(title: String, nodes: [SidebarNode] = [], mountables: [SidebarNode] = []) {
        self.title = title
        self.nodes = nodes
        self.mountables = mountables
    }
}

public struct SidebarNode: Equatable {
    public var title: String
    public var location: Location?
    public init(title: String, location: Location? = nil) {
        self.title = title
        self.location = location
    }
}

/// Pinned locations, persisted as canonical URIs so a remote or missing location keeps the
/// identity it was pinned with. A pin that fails validation on load is skipped, not fatal.
public final class PinStore {
    private let file: URL?
    public private(set) var pins: [Location] = []
    public private(set) var notices: [RecoverableStateNotice] = []

    public init(file: URL? = nil) {
        self.file = file
        guard let file, let data = try? BoundedState.read(file), let state = try? SessionCodec.decodeState(data) else { return }
        pins = state.locations
        notices = state.notices
    }

    public func pin(_ location: Location) {
        if !pins.contains(location) { pins.append(location) }
    }

    public func unpin(_ location: Location) { pins.removeAll { $0 == location } }

    public func save() throws {
        guard let file else { return }
        try BoundedState.write(try SessionCodec.encode(pins), to: file)
    }
}
