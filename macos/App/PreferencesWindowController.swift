import AppKit
import SwiftUI
import ExplorerAdapters
import ExplorerDomain

/// Settings: what opens, how conflicts are handled, search privacy, default view options and the
/// administrator helper. Every change is saved at once.
final class PreferencesModel: ObservableObject {
    private let services: AppServices
    @Published var preferences: ApplicationPreferences
    @Published var defaults: ViewOptions
    @Published var helperStatus: ElevationClient.Status
    @Published var message = ""

    init(services: AppServices) {
        self.services = services
        preferences = services.preferences.preferences
        defaults = services.viewPreferences.globalDefaults
        helperStatus = services.elevation.status
    }

    func update(_ change: (inout ApplicationPreferences) -> Void) {
        try? services.preferences.update(change)
        preferences = services.preferences.preferences
        services.operations.setRetentionDays(preferences.operationHistoryDays)
        try? services.searchHistory.enforcePolicy(enabled: preferences.recordSearchHistory, retentionDays: preferences.searchHistoryRetentionDays)
        try? services.searchHistory.save()
    }

    func updateDefaults(_ change: (inout ViewOptions) -> Void) {
        var copy = defaults
        change(&copy)
        services.viewPreferences.setDefaults(copy)
        try? services.viewPreferences.save()
        defaults = services.viewPreferences.globalDefaults
    }

    func clearSearchHistory() {
        services.searchHistory.clear()
        try? services.searchHistory.save()
        message = "Search history cleared."
    }

    func clearFolderViews() {
        let count = services.viewPreferences.clearAllFolders()
        try? services.viewPreferences.save()
        message = "Reset the view of \(count) folder\(count == 1 ? "" : "s")."
    }

    func clearOperationHistory() {
        services.operations.clearCompleted()
        message = "Operation history cleared."
    }

    func enableHelper() {
        do { try services.elevation.register() } catch { message = "The helper couldn’t be enabled: \(error.localizedDescription)" }
        helperStatus = services.elevation.status
        if helperStatus == .requiresApproval { services.elevation.openApprovalSettings() }
    }

    func refreshHelper() { helperStatus = services.elevation.status }
}

private struct PreferencesView: View {
    @ObservedObject var model: PreferencesModel

    var body: some View {
        TabView {
            general.tabItem { Label("General", systemImage: "gearshape") }
            search.tabItem { Label("Search", systemImage: "magnifyingglass") }
            view.tabItem { Label("View", systemImage: "square.grid.2x2") }
            helper.tabItem { Label("Administrator", systemImage: "lock.shield") }
        }
        .padding(20)
        .frame(width: 520, height: 420)
        .overlay(alignment: .bottom) { if !model.message.isEmpty { Text(model.message).font(.caption).foregroundStyle(.secondary).padding(.bottom, 4) } }
    }

    private var general: some View {
        Form {
            Picker("New tabs open", selection: Binding(get: { model.preferences.newTabLocation == Location.home.uri ? "home" : "other" }, set: { _ in model.update { $0.newTabLocation = Location.home.uri } })) {
                Text("Your home folder").tag("home")
                if model.preferences.newTabLocation != Location.home.uri { Text(Location.displayText(model.preferences.newTabLocation)).tag("other") }
            }
            Button("Use the Current Folder of the Front Window") {
                if let location = NSApp.keyWindow?.windowController.flatMap({ ($0 as? ExplorerWindowController)?.tab.location }) { model.update { $0.newTabLocation = location.uri } }
            }
            Toggle("Restore the previous tabs when opening", isOn: Binding(get: { model.preferences.restorePreviousTabs }, set: { value in model.update { $0.restorePreviousTabs = value } }))
            Toggle("Quit when the last tab is closed", isOn: Binding(get: { model.preferences.closeOnLastTab }, set: { value in model.update { $0.closeOnLastTab = value } }))
            Picker("When a name already exists", selection: Binding(get: { model.preferences.conflictDefault }, set: { value in model.update { $0.conflictDefault = value } })) {
                Text("Ask me").tag(ConflictDefault.ask)
                Text("Keep both").tag(ConflictDefault.keepBoth)
                Text("Skip").tag(ConflictDefault.skip)
                Text("Replace").tag(ConflictDefault.replace)
            }
            Stepper("Keep operation history for \(model.preferences.operationHistoryDays) day\(model.preferences.operationHistoryDays == 1 ? "" : "s")",
                    value: Binding(get: { model.preferences.operationHistoryDays }, set: { value in model.update { $0.operationHistoryDays = value } }), in: 1...90)
            Button("Clear Operation History") { model.clearOperationHistory() }
        }
        .formStyle(.grouped)
    }

    private var search: some View {
        Form {
            Toggle("Search inside files (uses Spotlight)", isOn: Binding(get: { model.preferences.contentSearchEnabled }, set: { value in model.update { $0.contentSearchEnabled = value } }))
            Text("File contents are searched only in folders Spotlight has indexed.").font(.caption).foregroundStyle(.secondary)
            Toggle("Remember what I search for", isOn: Binding(get: { model.preferences.recordSearchHistory }, set: { value in model.update { $0.recordSearchHistory = value } }))
            Stepper("Forget searches after \(model.preferences.searchHistoryRetentionDays) days",
                    value: Binding(get: { model.preferences.searchHistoryRetentionDays }, set: { value in model.update { $0.searchHistoryRetentionDays = value } }), in: 1...365)
                .disabled(!model.preferences.recordSearchHistory)
            Button("Clear Search History") { model.clearSearchHistory() }
        }
        .formStyle(.grouped)
    }

    private var view: some View {
        Form {
            Picker("Default view", selection: Binding(get: { model.defaults.viewMode }, set: { value in model.updateDefaults { $0.viewMode = value } })) {
                ForEach(ViewMode.allCases, id: \.self) { Text(ExplorerWindowController.name(for: $0)).tag($0) }
            }
            Picker("Icon size", selection: Binding(get: { model.defaults.gridSize }, set: { value in model.updateDefaults { $0.gridSize = value } })) {
                ForEach(GridSize.allCases, id: \.self) { Text($0.rawValue.replacingOccurrences(of: "-", with: " ").capitalized).tag($0) }
            }
            Picker("Sort by", selection: Binding(get: { model.defaults.sortField }, set: { value in model.updateDefaults { $0.sortField = value } })) {
                ForEach(SortField.allCases, id: \.self) { Text(ExplorerWindowController.sortName($0)).tag($0) }
            }
            Toggle("Show hidden files", isOn: Binding(get: { model.defaults.showHidden }, set: { value in model.updateDefaults { $0.showHidden = value } }))
            Toggle("Show thumbnails", isOn: Binding(get: { model.defaults.thumbnails }, set: { value in model.updateDefaults { $0.thumbnails = value } }))
            Toggle("List folders before files", isOn: Binding(get: { model.defaults.foldersFirst }, set: { value in model.updateDefaults { $0.foldersFirst = value } }))
            Button("Reset Every Folder’s Own View") { model.clearFolderViews() }
        }
        .formStyle(.grouped)
    }

    private var helper: some View {
        Form {
            Text("Some changes need administrator permission, such as moving a file into a protected folder. Morin Explorer never runs as administrator; a small helper does one approved task at a time.")
                .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            LabeledContent("Helper") { Text(Self.describe(model.helperStatus)) }
            switch model.helperStatus {
            case .notRegistered: Button("Enable Helper…") { model.enableHelper() }
            case .requiresApproval: Button("Open System Settings…") { model.enableHelper() }
            case .enabled, .unavailable: EmptyView()
            }
            Button("Check Again") { model.refreshHelper() }
        }
        .formStyle(.grouped)
    }

    private static func describe(_ status: ElevationClient.Status) -> String {
        switch status {
        case .notRegistered: return "Not enabled"
        case .requiresApproval: return "Waiting for approval in System Settings"
        case .enabled: return "Enabled"
        case .unavailable: return "Not included in this build"
        }
    }
}

final class PreferencesWindowController: NSWindowController {
    init(services: AppServices) {
        let model = PreferencesModel(services: services)
        let host = NSHostingController(rootView: PreferencesView(model: model))
        let window = NSWindow(contentViewController: host)
        window.title = "Settings"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier("preferences-window")
        super.init(window: window)
        window.center()
    }

    required init?(coder: NSCoder) { fatalError() }
}

private extension Location {
    static func displayText(_ uri: String) -> String { (try? Location(uri: uri))?.displayName ?? uri }
}
