import Foundation
import UniformTypeIdentifiers
import ExplorerDomain

/// Content types for the type table (`FileTypeTable`). macOS identifies most extensions
/// (`public.zip-archive`, `org.7-zip.7-zip-archive`, the Office and OpenDocument types), but not a
/// compound extension such as `.tar.bz2` (it sees only `.bz2`) nor RAR or APK (dynamic types). For
/// those the adapter names the type by MIME, as the table does.
public enum ContentTypes {
    private static let compound: [(suffix: String, type: String)] = [
        (".tar.gz", "org.gnu.gnu-zip-tar-archive"), (".tgz", "org.gnu.gnu-zip-tar-archive"),
        (".tar.bz2", "application/x-bzip-compressed-tar"), (".tbz2", "application/x-bzip-compressed-tar"),
        (".tar.xz", "application/x-xz-compressed-tar"), (".txz", "application/x-xz-compressed-tar"),
        (".tar.lzma", "application/x-lzma-compressed-tar"), (".tar.zst", "application/x-zstd-compressed-tar"),
        (".rar", "application/vnd.rar"), (".apk", "application/vnd.android.package-archive"),
    ]

    public static func contentType(forName name: String, isDirectory: Bool) -> String {
        if isDirectory { return UTType.folder.identifier }
        let lower = name.lowercased()
        for entry in compound where lower.hasSuffix(entry.suffix) { return entry.type }
        let ext = (lower as NSString).pathExtension
        if !ext.isEmpty, let type = UTType(filenameExtension: ext), !type.isDynamic { return type.identifier }
        return FileItem.placeholderContentType
    }
}
