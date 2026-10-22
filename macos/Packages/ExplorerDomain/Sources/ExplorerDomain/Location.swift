import Foundation

/// URI input was not safe to retain as application state.
public enum LocationError: Error, Equatable, CustomStringConvertible {
    case unsafeLocation
    public var description: String { "unsafe location" }
}

/// A validated, canonical URI. Equality and hashing are by canonical text.
///
/// The canonical form folds the scheme and host to lower case, drops a scheme's default port,
/// decodes only unreserved bytes (`%7e` becomes `~`), upper-cases every other escape (`%2f` becomes
/// `%2F`, so an encoded slash stays distinct from a separator), and rejects credentials, queries,
/// fragments, bad escapes and control characters. An authority that carries percent-encoding is
/// identity rather than a host name (an archive's own escaped URI) and keeps its case.
public struct Location: Hashable, CustomStringConvertible {
    public let uri: String

    /// Creates a location from validated URI input.
    public init(uri: String) throws {
        self.uri = try Location.canonicalize(uri)
    }

    /// A local `file:` location at an explicit filesystem boundary. `path` must be absolute.
    public init(localPath path: String) {
        // Escapes the way a filename-to-URI conversion does, then runs the ordinary canonical form.
        let escaped = URIText.escape(path, allowing: URIText.pathAllowed)
        self.uri = (try? Location.canonicalize("file://" + escaped)) ?? "file://" + escaped
    }

    /// The current user's home directory.
    public static var home: Location {
        Location(localPath: ProcessInfo.processInfo.environment["HOME"] ?? NSHomeDirectory())
    }

    public var description: String { uri }

    // MARK: Parts

    /// The raw parts of the canonical URI.
    struct Parts {
        var scheme: String
        var authority: String?
        var path: String
    }

    var parts: Parts { Location.split(uri) ?? Parts(scheme: "", authority: nil, path: uri) }

    /// The path of a `file:` location as the filesystem sees it. A remote URI has none, even
    /// where a mount projects one: a projection is not local identity.
    public var localPath: String? {
        guard uri.hasPrefix("file:") else { return nil }
        let path = URIText.unescape(parts.path)
        return path.hasPrefix("/") ? path : nil
    }

    public var isLocal: Bool { uri.hasPrefix("file:") }

    /// Whether this location is inside an archive.
    public var isInsideArchive: Bool { uri.hasPrefix("archive://") }

    // MARK: Archives

    /// Where this archive's contents live: the archive's own URI, escaped twice (once to fit in
    /// an authority, once because parsing unescapes it), as the authority of an `archive://` URI.
    public func archiveContents() throws -> Location {
        let once = URIText.escape(uri, allowing: URIText.unreserved)
        let twice = URIText.escape(once, allowing: URIText.unreserved)
        return try Location(uri: "archive://\(twice)/")
    }

    /// The archive file a location inside an archive was opened from, at any depth.
    public var archiveSource: Location? {
        guard isInsideArchive, let authority = parts.authority else { return nil }
        let original = URIText.unescape(URIText.unescape(authority))
        return try? Location(uri: original)
    }

    /// The decoded path of this location inside its archive; empty at the root.
    public var archiveInnerPath: String? {
        guard isInsideArchive else { return nil }
        return URIText.unescape(parts.path).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    // MARK: Navigation

    /// The folder containing this location. At the root of an archive that is the folder the
    /// archive is in, the way leaving any other folder works.
    public var parent: Location? {
        let parts = self.parts
        let trimmed = parts.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if trimmed.isEmpty {
            if isInsideArchive { return archiveSource?.parent }
            return nil
        }
        var segments = trimmed.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        segments.removeLast()
        let path = "/" + segments.joined(separator: "/")
        if isLocal { return Location(localPath: URIText.unescape(path)) }
        let authority = parts.authority.map { "//" + $0 } ?? ""
        return try? Location(uri: "\(parts.scheme):\(authority)\(path)")
    }

    /// Whether this location is `root` or somewhere below it.
    public func isWithin(_ root: Location) -> Bool {
        if self == root { return true }
        let rootParts = root.parts, mine = parts
        guard rootParts.scheme == mine.scheme, rootParts.authority == mine.authority else { return false }
        let base = rootParts.path.hasSuffix("/") ? rootParts.path : rootParts.path + "/"
        return mine.path.hasPrefix(base)
    }

    /// A child by plain name (the name is escaped, not interpreted).
    public func child(named name: String) throws -> Location {
        let parts = self.parts
        let base = parts.path.hasSuffix("/") ? parts.path : parts.path + "/"
        let escaped = URIText.escape(name, allowing: URIText.pathAllowed.subtracting([UInt8(ascii: "/")]))
        let authority = parts.authority.map { "//" + $0 } ?? ""
        return try Location(uri: "\(parts.scheme):\(authority)\(base)\(escaped)")
    }

    // MARK: Display

    /// A concise label for a tab, breadcrumb or sidebar row.
    public var displayName: String {
        if let name = archiveDisplayName { return name }
        if let path = localPath {
            let last = path.split(separator: "/").last.map(String.init)
            return (last?.isEmpty == false ? last : nil) ?? path
        }
        return redacted
    }

    /// A location inside an archive is named after the archive at its root and after the current
    /// entry below it. Redaction exists to keep a remote share's path out of the interface, but an
    /// archive's authority is a file the user picked and its segments are its own contents.
    private var archiveDisplayName: String? {
        guard isInsideArchive else { return nil }
        let segments = parts.path.split(separator: "/", omittingEmptySubsequences: true)
        if let last = segments.last { return URIText.unescape(String(last)) }
        guard let archive = archiveSource else { return nil }
        let name = archive.displayName
        return name.isEmpty ? archive.redacted : name
    }

    /// A user-safe description that never includes remote path details.
    public var redacted: String {
        let parts = self.parts
        guard let authority = parts.authority else { return "\(parts.scheme):…" }
        let host = Location.hostAndPort(authority)?.host ?? authority
        return "\(parts.scheme)://\(host)/…"
    }

    // MARK: Canonicalisation

    static func split(_ text: String) -> Parts? {
        guard let colon = text.firstIndex(of: ":") else { return nil }
        let scheme = String(text[text.startIndex..<colon])
        guard let first = scheme.unicodeScalars.first, first.properties.isAlphabetic, first.isASCII,
              scheme.unicodeScalars.allSatisfy({ $0.isASCII && ($0.properties.isAlphabetic || ("0"..."9").contains($0) || "+-.".unicodeScalars.contains($0)) })
        else { return nil }
        var rest = text[text.index(after: colon)...]
        var authority: String?
        if rest.hasPrefix("//") {
            rest = rest.dropFirst(2)
            let end = rest.firstIndex(where: { $0 == "/" || $0 == "?" || $0 == "#" }) ?? rest.endIndex
            authority = String(rest[rest.startIndex..<end])
            rest = rest[end...]
        }
        return Parts(scheme: scheme, authority: authority, path: String(rest))
    }

    /// Splits `host[:port]`, including a bracketed IPv6 host. Nil for anything malformed.
    static func hostAndPort(_ authority: String) -> (host: String, port: Int?)? {
        if authority.hasPrefix("[") {
            guard let close = authority.firstIndex(of: "]") else { return nil }
            let host = String(authority[authority.startIndex...close])
            let tail = authority[authority.index(after: close)...]
            if tail.isEmpty { return (host, nil) }
            guard tail.hasPrefix(":"), let port = Int(tail.dropFirst()) else { return nil }
            return (host, port)
        }
        if let colon = authority.lastIndex(of: ":") {
            let host = String(authority[authority.startIndex..<colon])
            let portText = authority[authority.index(after: colon)...]
            if portText.isEmpty { return (host, nil) }
            guard let port = Int(portText), port >= 0, port <= 65535 else { return nil }
            return (host, port)
        }
        return (authority, nil)
    }

    static func canonicalize(_ input: String) throws -> String {
        if input.isEmpty || input.utf8.contains(where: { $0 < 0x20 || $0 == 0x7f }) { throw LocationError.unsafeLocation }
        try URIText.validateEscapes(input)
        guard let raw = split(input) else { throw LocationError.unsafeLocation }
        // A query or fragment, even an empty one, is not part of a location.
        if raw.path.contains("?") || raw.path.contains("#") { throw LocationError.unsafeLocation }
        let scheme = raw.scheme.lowercased()
        let path = try URIText.normalize(raw.path)
        guard let authority = raw.authority else { return "\(scheme):\(path)" }
        if authority.contains("@") { throw LocationError.unsafeLocation }
        // An authority that carries percent-encoding is identity, not a host name: an archive is
        // addressed as `archive://<the archive's escaped URI>/`. Lower-casing it would lose the
        // case of the archive's own path. Real host names are still folded.
        if authority.contains("%"), !authority.contains(":"), !authority.contains("[") {
            return "\(scheme)://\(try URIText.normalize(authority))\(path)"
        }
        guard let (host, port) = hostAndPort(authority) else { throw LocationError.unsafeLocation }
        var text = "\(scheme)://\(host.lowercased())"
        if let port, !isDefaultPort(scheme: scheme, port: port) { text += ":\(port)" }
        return text + path
    }

    static func isDefaultPort(scheme: String, port: Int) -> Bool {
        switch (scheme, port) {
        case ("http", 80), ("https", 443), ("ftp", 21), ("sftp", 22), ("ssh", 22), ("smb", 445): return true
        default: return false
        }
    }
}

/// The text entry boundary: an absolute local path or a validated URI.
public enum LocationInput {
    public static func parse(_ input: String) throws -> Location {
        if input.isEmpty || input.unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7f }) {
            throw LocationError.unsafeLocation
        }
        if input.hasPrefix("/") { return Location(localPath: input) }
        return try Location(uri: input)
    }
}

/// A requested location together with the location a provider ultimately reached.
public struct ResolvedLocation: Hashable {
    public let requested: Location
    public let resolved: Location
    public init(requested: Location, resolved: Location) {
        self.requested = requested
        self.resolved = resolved
    }
}

/// Percent-encoding helpers shared by the location code.
enum URIText {
    static let unreserved: Set<UInt8> = Set(Array("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~".utf8))
    static let pathAllowed: Set<UInt8> = unreserved.union(Array("!$&'()*+,;=:@/".utf8))

    static func escape(_ text: String, allowing allowed: Set<UInt8>) -> String {
        var out = ""
        for byte in text.utf8 {
            if allowed.contains(byte) { out.unicodeScalars.append(Unicode.Scalar(byte)) } else { out += String(format: "%%%02X", byte) }
        }
        return out
    }

    /// Decodes every valid `%XX`; invalid sequences are left as they are.
    static func unescape(_ text: String) -> String {
        var bytes: [UInt8] = []
        let input = Array(text.utf8)
        var i = 0
        while i < input.count {
            if input[i] == UInt8(ascii: "%"), i + 2 < input.count,
               let high = hexValue(input[i + 1]), let low = hexValue(input[i + 2]) {
                bytes.append(UInt8(high * 16 + low))
                i += 3
            } else {
                bytes.append(input[i])
                i += 1
            }
        }
        return String(decoding: bytes, as: UTF8.self)
    }

    static func hexValue(_ byte: UInt8) -> Int? {
        switch byte {
        case UInt8(ascii: "0")...UInt8(ascii: "9"): return Int(byte - UInt8(ascii: "0"))
        case UInt8(ascii: "a")...UInt8(ascii: "f"): return Int(byte - UInt8(ascii: "a")) + 10
        case UInt8(ascii: "A")...UInt8(ascii: "F"): return Int(byte - UInt8(ascii: "A")) + 10
        default: return nil
        }
    }

    /// Every `%` must be followed by two hex digits, with at least one byte after them.
    static func validateEscapes(_ text: String) throws {
        let bytes = Array(text.utf8)
        var i = 0
        while i < bytes.count {
            if bytes[i] == UInt8(ascii: "%") {
                if i + 2 >= bytes.count || hexValue(bytes[i + 1]) == nil || hexValue(bytes[i + 2]) == nil {
                    throw LocationError.unsafeLocation
                }
                i += 3
            } else {
                i += 1
            }
        }
    }

    /// Decodes unreserved escapes, upper-cases the rest.
    static func normalize(_ text: String) throws -> String {
        let bytes = Array(text.utf8)
        var out: [UInt8] = []
        var i = 0
        while i < bytes.count {
            if bytes[i] != UInt8(ascii: "%") {
                out.append(bytes[i])
                i += 1
                continue
            }
            guard i + 2 < bytes.count, let high = hexValue(bytes[i + 1]), let low = hexValue(bytes[i + 2]) else {
                throw LocationError.unsafeLocation
            }
            let value = UInt8(high * 16 + low)
            if unreserved.contains(value) {
                out.append(value)
            } else {
                out.append(UInt8(ascii: "%"))
                out.append(contentsOf: String(format: "%02X", value).utf8)
            }
            i += 3
        }
        return String(decoding: out, as: UTF8.self)
    }
}
