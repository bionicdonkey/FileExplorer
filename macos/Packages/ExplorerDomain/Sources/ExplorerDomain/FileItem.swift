import Foundation

/// Metadata needed to render one row or tile of a directory view. Built from an owned snapshot,
/// without I/O.
public struct FileItem: Equatable {
    public let location: Location
    public let capabilities: CapabilitySnapshot
    public let name: String
    public let isDirectory: Bool
    public let size: UInt64
    public let modified: Date
    public let contentType: String
    /// Whether the content type came from the listing or visible metadata rather than the
    /// placeholder used while the query is pending.
    public let contentTypeResolved: Bool

    public static let placeholderContentType = "application/octet-stream"

    public init(snapshot entry: EntrySnapshot) {
        location = entry.location
        capabilities = entry.capabilities
        name = entry.displayName
        isDirectory = entry.kind == .directory || entry.kind == .shortcut
        size = entry.size ?? 0
        modified = entry.modified ?? Date(timeIntervalSince1970: 0)
        contentType = entry.contentType ?? FileItem.placeholderContentType
        contentTypeResolved = entry.contentType != nil
    }

    private init(copying item: FileItem, size: UInt64, modified: Date, contentType: String) {
        location = item.location
        capabilities = item.capabilities
        name = item.name
        isDirectory = item.isDirectory
        self.size = size
        self.modified = modified
        self.contentType = contentType
        contentTypeResolved = true
    }

    /// The canonical URI used for selection, operations and drag and drop.
    public var identityKey: String { location.uri }

    /// Incorporates attributes that arrive after a row is bound. Identity and capabilities never change.
    public func withVisibleMetadata(_ metadata: VisibleMetadata) -> FileItem {
        FileItem(copying: self, size: metadata.size ?? size, modified: metadata.modified ?? modified,
                 contentType: metadata.contentType ?? contentType)
    }
}
