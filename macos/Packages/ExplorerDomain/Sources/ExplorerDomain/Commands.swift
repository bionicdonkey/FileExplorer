import Foundation

public enum ClipboardAction: String, Equatable { case copy, cut }

public struct ClipboardPayload: Equatable {
    public var action: ClipboardAction
    public var locations: [Location]

    public init(action: ClipboardAction, locations: [Location]) {
        self.action = action
        self.locations = locations
    }

    /// A URI list: one location per line, `#` comments allowed, duplicates dropped, and the whole
    /// payload rejected if any line is unsafe or the list is empty.
    public static func parse(action: ClipboardAction, text: String) throws -> ClipboardPayload {
        ClipboardPayload(action: action, locations: try parseURIList(text))
    }

    /// The GNOME copied-files format: `copy` or `cut` on the first line, then the URI list.
    public static func parseGnome(_ text: String) throws -> ClipboardPayload {
        // `"\r\n"` is a single Character in Swift, so look for the newline at the UTF-16 level.
        guard let newline = text.range(of: "\n") else { throw LocationError.unsafeLocation }
        let head = text[text.startIndex..<newline.lowerBound].trimmingCharacters(in: CharacterSet(charactersIn: "\r"))
        let action: ClipboardAction
        switch head {
        case "copy": action = .copy
        case "cut": action = .cut
        default: throw LocationError.unsafeLocation
        }
        return try parse(action: action, text: String(text[newline.upperBound...]))
    }

    public var uriList: String { locations.map { "\($0.uri)\r\n" }.joined() }
}

public func parseURIList(_ text: String) throws -> [Location] {
    var seen = Set<Location>()
    var locations: [Location] = []
    // Split on UTF-16 newlines: "\r\n" is one Character in Swift and would not split on "\n".
    for rawLine in text.components(separatedBy: "\n") {
        var line = rawLine
        if line.last == "\r" { line.removeLast() }
        if line.isEmpty || line.hasPrefix("#") { continue }
        let location = try Location(uri: line)
        if seen.insert(location).inserted { locations.append(location) }
    }
    if locations.isEmpty { throw LocationError.unsafeLocation }
    return locations
}

/// The in-app clipboard. A cut leaves it only when the paste started for it finishes terminally.
public final class ClipboardState {
    public private(set) var payload: ClipboardPayload?
    public private(set) var generation: UInt64 = 0
    private var pending: UInt64?
    private static var nextGeneration: UInt64 = 1

    public init() {}

    public func replace(_ payload: ClipboardPayload) {
        generation = ClipboardState.nextGeneration
        ClipboardState.nextGeneration += 1
        self.payload = payload
        pending = nil
    }

    public func clear() {
        payload = nil
        pending = nil
    }

    public func beginPaste(_ id: UInt64) { pending = id }

    public func finishPaste(_ id: UInt64, terminal: Bool) {
        guard terminal, pending == id else { return }
        pending = nil
        if payload?.action == .cut { payload = nil }
    }
}

/// A new file or folder name must be one plain component: empty, `.`, `..`, separators and
/// control characters are refused, and the name is escaped rather than interpreted.
public func namedChild(of parent: Location, name: String) throws -> Location {
    if name.isEmpty || name == "." || name == ".." || name.contains("/") || name.unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7f }) {
        throw LocationError.unsafeLocation
    }
    return try parent.child(named: name)
}

/// What a transfer to `destination` can do with each source: an item moved to the folder it is
/// already in, or anything dropped on or inside itself, has nowhere to go. A copy into the folder
/// it came from stays in, as a duplicate.
public func transferSources(_ sources: [Location], destination: Location, action: ClipboardAction) -> [Location] {
    sources.filter { source in
        !destination.isWithin(source) && (action == .copy || source.parent != destination)
    }
}

/// `("report", ".txt")`; a compressed tar keeps both parts as its extension.
public func splitExtension(_ name: String) -> (stem: String, ext: String) {
    guard let dot = name.lastIndex(of: "."), dot > name.startIndex else { return (name, "") }
    var cut = dot
    if name[..<dot].hasSuffix(".tar") { cut = name.index(dot, offsetBy: -4) }
    return (String(name[..<cut]), String(name[cut...]))
}

func stripCopySuffix(_ stem: String) -> String {
    guard let open = stem.range(of: " (copy", options: .backwards) else { return stem }
    let tail = stem[open.upperBound...]
    let numbered = tail.hasPrefix(" ") && tail.hasSuffix(")") && UInt32(tail.dropFirst().dropLast()) != nil
    return (tail == ")" || numbered) ? String(stem[..<open.lowerBound]) : stem
}

/// A name beside `source` for a duplicate: `report (copy).txt`, then `report (copy 2).txt`.
/// Folders keep their whole name, and a duplicate of a duplicate numbers on from the original.
public func copySibling(of source: Location, isDirectory: Bool, exists: (Location) -> Bool) throws -> Location {
    guard let parent = source.parent else { throw LocationError.unsafeLocation }
    let name = source.displayName
    let (stem, ext) = isDirectory ? (name, "") : splitExtension(name)
    let base = stripCopySuffix(stem)
    var number = 1
    while true {
        let label = number == 1 ? "(copy)" : "(copy \(number))"
        let candidate = try namedChild(of: parent, name: "\(base) \(label)\(ext)")
        if !exists(candidate) { return candidate }
        number += 1
    }
}

/// A drag or paste that has been checked against the capabilities of its items and destination.
public struct DragProposal: Equatable {
    public var sources: [Location]
    public var destination: Location
    public var action: ClipboardAction

    public static func negotiate(items: [FileItem], destination: Location, capabilities: CapabilitySnapshot, action: ClipboardAction) -> DragProposal? {
        let required: Capability = action == .copy ? .copy : .move
        guard !items.isEmpty, capabilities.supports(.create), items.allSatisfy({ $0.capabilities.supports(required) }) else { return nil }
        return DragProposal(sources: items.map(\.location), destination: destination, action: action)
    }
}
