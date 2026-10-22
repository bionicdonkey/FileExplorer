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
