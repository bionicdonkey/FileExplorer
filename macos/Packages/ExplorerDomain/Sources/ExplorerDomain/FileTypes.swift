import Foundation

/// What opening an entry does. Rules match by exact type, never by subtype: a zip-based document
/// is a subtype of zip, and a subtype test would send a report to the archive browser instead of
/// the application that owns it. This table mirrors `spec/contracts/file-types.json`; a scenario
/// checks every type and extension in the contract against it.
public struct FileTypeRule {
    public enum Browsable { case always, never, localOnly }
    public let id: String
    public let types: Set<String>
    public let extensions: [String]
    public let browsable: Browsable
    public let extractable: Bool
}

public enum FileTypeTable {
    public static let rules: [FileTypeRule] = [
        FileTypeRule(
            id: "zip-based-documents",
            types: [
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "application/vnd.oasis.opendocument.text", "application/epub+zip", "application/vnd.android.package-archive",
                "org.openxmlformats.wordprocessingml.document", "org.openxmlformats.spreadsheetml.sheet",
                "org.openxmlformats.presentationml.presentation", "org.oasis-open.opendocument.text",
                "org.idpf.epub-container", "com.android.package-archive",
            ],
            extensions: ["docx", "xlsx", "pptx", "odt", "epub", "apk"], browsable: .never, extractable: false),
        FileTypeRule(
            id: "browsable-archives",
            types: [
                "application/zip", "application/x-zip-compressed", "application/vnd.rar", "application/x-rar",
                "application/x-rar-compressed", "application/x-tar", "application/x-compressed-tar",
                "application/x-bzip-compressed-tar", "application/x-bzip2-compressed-tar", "application/x-xz-compressed-tar",
                "application/x-lzma-compressed-tar", "application/x-zstd-compressed-tar", "application/x-tarz",
                "application/x-cpio", "application/x-xar",
                "public.zip-archive", "public.tar-archive", "org.gnu.gnu-zip-tar-archive", "public.cpio-archive", "com.apple.xar-archive",
            ],
            extensions: ["zip", "tar", "tar.gz", "tgz", "tar.bz2", "tar.xz", "rar", "cpio"], browsable: .always, extractable: true),
        FileTypeRule(
            id: "seven-zip",
            types: ["application/x-7z-compressed", "application/x-7z-compressed-tar", "org.7-zip.7-zip-archive"],
            extensions: ["7z"], browsable: .localOnly, extractable: true),
    ]

    /// The rule for a resolved content type, or, while the type has not arrived yet, for the type
    /// the file name implies. An unresolved name with no known extension matches nothing.
    public static func rule(for item: FileItem) -> FileTypeRule? {
        if item.contentTypeResolved { return rules.first { $0.types.contains(item.contentType) } }
        let lower = item.name.lowercased()
        // Longer extensions first, so `x.tar.gz` is a tar.gz, not a `.gz`.
        let candidates = rules.flatMap { rule in rule.extensions.map { (rule, $0) } }.sorted { $0.1.count > $1.1.count }
        return candidates.first { lower.hasSuffix("." + $0.1) }?.0
    }
}

public enum Activation: Equatable {
    /// Show this location in the window, the way a folder is shown.
    case browse(Location)
    /// Hand the file to whatever the system has registered for its type.
    case launchExternally
}

public func activation(for item: FileItem) -> Activation {
    if item.isDirectory { return .browse(item.location) }
    guard let rule = FileTypeTable.rule(for: item) else { return .launchExternally }
    let browsable: Bool
    switch rule.browsable {
    case .always: browsable = true
    case .never: browsable = false
    // Read from its own local file, because the reader has to seek.
    case .localOnly: browsable = item.location.localPath != nil
    }
    if browsable, let contents = try? item.location.archiveContents() { return .browse(contents) }
    return .launchExternally
}

/// Whether Extract is offered: every archive the window can browse, including 7-Zip, but never an
/// archive shown inside another archive.
public func isExtractable(_ item: FileItem) -> Bool {
    if item.location.isInsideArchive || item.isDirectory { return false }
    return FileTypeTable.rule(for: item)?.extractable ?? false
}
