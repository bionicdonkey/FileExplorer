import CArchive

/// The system libarchive the archive adapters use.
public enum ArchiveLibrary {
    public static var version: String { String(cString: archive_version_string()) }
}
