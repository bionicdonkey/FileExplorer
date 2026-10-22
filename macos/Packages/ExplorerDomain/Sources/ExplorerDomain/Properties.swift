import Foundation

public struct PermissionBits: Equatable {
    public var ownerRead = false, ownerWrite = false, ownerExecute = false
    public var groupRead = false, groupWrite = false, groupExecute = false
    public var otherRead = false, otherWrite = false, otherExecute = false

    public init() {}

    public init(mode: UInt16) {
        ownerRead = mode & 0o400 != 0; ownerWrite = mode & 0o200 != 0; ownerExecute = mode & 0o100 != 0
        groupRead = mode & 0o040 != 0; groupWrite = mode & 0o020 != 0; groupExecute = mode & 0o010 != 0
        otherRead = mode & 0o004 != 0; otherWrite = mode & 0o002 != 0; otherExecute = mode & 0o001 != 0
    }

    public var mode: UInt16 {
        (ownerRead ? 0o400 : 0) | (ownerWrite ? 0o200 : 0) | (ownerExecute ? 0o100 : 0)
            | (groupRead ? 0o040 : 0) | (groupWrite ? 0o020 : 0) | (groupExecute ? 0o010 : 0)
            | (otherRead ? 0o004 : 0) | (otherWrite ? 0o002 : 0) | (otherExecute ? 0o001 : 0)
    }
}

/// What the properties window shows for one local item. Fields a platform cannot provide are nil
/// and shown as unavailable rather than guessed.
public struct PropertyDetails: Equatable {
    public var path: String
    public var name: String
    public var isDirectory: Bool
    public var contentType: String
    public var size: UInt64
    public var created: Date?
    public var modified: Date?
    public var accessed: Date?
    public var owner: String
    public var group: String
    public var permissions: PermissionBits

    public init(path: String, name: String, isDirectory: Bool, contentType: String, size: UInt64, created: Date? = nil, modified: Date? = nil,
                accessed: Date? = nil, owner: String, group: String, permissions: PermissionBits) {
        self.path = path
        self.name = name
        self.isDirectory = isDirectory
        self.contentType = contentType
        self.size = size
        self.created = created
        self.modified = modified
        self.accessed = accessed
        self.owner = owner
        self.group = group
        self.permissions = permissions
    }
}

public enum ByteFormat {
    /// A byte count with binary units, compact: `1.5 MiB`.
    public static func string(_ bytes: UInt64) -> String {
        let units = ["B", "KiB", "MiB", "GiB", "TiB"]
        var value = Double(bytes)
        var index = 0
        while value >= 1024 && index < units.count - 1 { value /= 1024; index += 1 }
        return index == 0 ? "\(bytes) B" : String(format: "%.1f %@", value, units[index])
    }
}
