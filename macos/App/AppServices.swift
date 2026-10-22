import AppKit
import ExplorerAdapters
import ExplorerDomain

/// Everything shared across windows and tabs: the repositories and operation backend, the single
/// operation manager (so progress and history are one list), preferences, clipboard, pins and
/// volumes. Launch arguments let tests and screenshots run against an isolated state folder:
///
///   -MorinStateDirectory <path>   keep preferences, history and pins there
///   -MorinOpen <path>             open this folder in the first window
///   -MorinSnapshot <path>         render the windows to <path>-<n>.png and quit
final class AppServices {
    static let shared = AppServices()

    let stateDirectory: URL
    let preferences: PreferencesStore
    let directories: DirectoryRepository
    let backend: FileOperations
    let operations: OperationManager
    let searchRepository: SearchRepository
    let clipboard = ClipboardState()
    /// The system pasteboard's change count right after our own copy; a different count means another application copied since.
    var clipboardChangeCount: Int?
    let sidebarPreferences: SidebarPreferences
    let viewPreferences: ViewPreferencesStore
    let searchHistory: SearchHistory
    let volumes: SystemVolumes
    let thumbnails = ThumbnailService()
    let elevation = ElevationClient()
    let launchLocation: String?
    let snapshotPath: String?
    let launchSearch: String?
    let launchShow: String?

    init(arguments: [String] = ProcessInfo.processInfo.arguments) {
        func value(after flag: String) -> String? { arguments.firstIndex(of: flag).flatMap { arguments.indices.contains($0 + 1) ? arguments[$0 + 1] : nil } }
        stateDirectory = value(after: "-MorinStateDirectory").map { URL(fileURLWithPath: $0, isDirectory: true) } ?? PreferencesStore.defaultDirectory
        launchLocation = value(after: "-MorinOpen")
        snapshotPath = value(after: "-MorinSnapshot")
        launchSearch = value(after: "-MorinSearch")
        launchShow = value(after: "-MorinShow")
        preferences = PreferencesStore(directory: stateDirectory)
        directories = DirectoryRouter()
        backend = ArchiveFileOperations(wrapping: LocalFileOperations())
        operations = OperationManager(backend: backend, capabilities: FileExecutor.localCapabilities, historyFile: preferences.historyFile,
                                      retentionDays: preferences.preferences.operationHistoryDays)
        searchRepository = LocalSearchRepository()
        sidebarPreferences = SidebarPreferences(file: stateDirectory.appendingPathComponent("sidebar.json"))
        viewPreferences = ViewPreferencesStore(file: preferences.viewOptionsFile)
        searchHistory = SearchHistory.load(from: preferences.searchHistoryFile)
        volumes = SystemVolumes()
        operations.changed = { NotificationCenter.default.post(name: .operationsChanged, object: nil) }
        try? searchHistory.enforcePolicy(enabled: preferences.preferences.recordSearchHistory, retentionDays: preferences.preferences.searchHistoryRetentionDays)
    }
}

extension Notification.Name {
    static let operationsChanged = Notification.Name("casa.alera.MorinExplorer.operationsChanged")
}
