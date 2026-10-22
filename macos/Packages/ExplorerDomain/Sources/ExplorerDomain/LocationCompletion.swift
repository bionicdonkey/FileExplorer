import Foundation

/// Folder suggestions for a location entry: the folder whose children can complete the typed text,
/// and the partial name to match inside it.
public struct CompletionTarget: Equatable {
    public static let maximumSuggestions = 50
    public static let maximumEnumerated = 4096

    public struct Suggestion: Equatable {
        public var name: String
        public var text: String
    }

    /// The folder to list, as typed up to and including its last separator.
    public let directory: Location
    public let directoryText: String
    public let prefix: String
    let uriStyle: Bool

    /// Nil when the text cannot be completed: empty, containing control characters, relative, a URI
    /// with no path below its authority, or a prefix carrying a query or fragment mark.
    public init?(_ input: String) {
        if input.isEmpty || input.unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7f }) { return nil }
        let directoryText: String, prefix: String, uriStyle: Bool
        if let scheme = input.range(of: "://") {
            // Without a path there is nothing below the authority to complete.
            guard input[scheme.upperBound...].contains("/"), let separator = input.lastIndex(of: "/") else { return nil }
            directoryText = String(input[...separator])
            // A tab or remote location supplies a URI, so the typed prefix may be percent-encoded.
            prefix = URIText.unescape(String(input[input.index(after: separator)...]))
            uriStyle = true
        } else {
            // Only absolute paths or URIs are accepted, so a relative spelling cannot be completed.
            guard input.hasPrefix("/"), let separator = input.lastIndex(of: "/") else { return nil }
            directoryText = String(input[...separator])
            prefix = String(input[input.index(after: separator)...])
            uriStyle = false
        }
        if prefix.contains("?") || prefix.contains("#") { return nil }
        guard let directory = uriStyle ? try? Location(uri: directoryText) : Location(localPath: directoryText) else { return nil }
        self.directory = directory
        self.directoryText = directoryText
        self.prefix = prefix
        self.uriStyle = uriStyle
    }

    /// Hiding dotfiles keeps the common case quiet; asking for one still finds it, as a shell does.
    public func matches(_ name: String) -> Bool {
        if name.hasPrefix(".") && !prefix.hasPrefix(".") { return false }
        return name.lowercased().hasPrefix(prefix.lowercased())
    }

    /// The text that replaces the entry's text when a suggestion is chosen: the folder, the name
    /// (escaped for a URI) and a trailing separator.
    public func suggestion(for name: String) -> Suggestion {
        let segment = uriStyle ? URIText.escape(name, allowing: URIText.unreserved.union([UInt8(ascii: "/")])) : name
        return Suggestion(name: name, text: "\(directoryText)\(segment)/")
    }

    /// Folders among `names` that match, sorted case-insensitively and capped.
    public func suggestions(from names: [String]) -> [Suggestion] {
        names.filter(matches).sorted { ($0.lowercased(), $0) < ($1.lowercased(), $1) }
            .prefix(CompletionTarget.maximumSuggestions).map(suggestion(for:))
    }
}
