import AppKit
import ExplorerDomain

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let services = AppServices.shared
    private lazy var windows = WindowManager(services: services)
    private var preferencesWindow: PreferencesWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build(appName: "Morin Explorer", delegate: self)
        let locations = startupLocations()
        for (index, location) in locations.enumerated() {
            if index == 0 { windows.open(location) } else { windows.open(location, asTabIn: windows.controllers.first?.window, activate: false) }
        }
        NSApp.activate(ignoringOtherApps: true)
        if let path = services.snapshotPath { scheduleSnapshot(to: path) }
    }

    /// `-MorinSnapshot <path>`: renders each window to `<path>-<n>.png` once it has loaded, then quits.
    /// It draws the window itself, so it needs no screen-recording permission; the tests and the
    /// visual checks on a machine nobody is sitting at use it.
    private func scheduleSnapshot(to path: String) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
            if let text = self.services.launchSearch, let controller = self.windows.controllers.first { controller.tab.beginSearch(text, mode: .filename) }
            switch self.services.launchShow {
            case "preferences": self.showPreferences(nil)
            case "properties": self.windows.controllers.first.map { $0.commands.showProperties($0.tab.displayed.first!) }
            default: break
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 7) {
            for (index, window) in NSApp.windows.filter({ $0.isVisible && $0.contentView != nil }).enumerated() {
                guard let frame = window.contentView?.superview, let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else { continue }
                frame.cacheDisplay(in: frame.bounds, to: bitmap)
                try? bitmap.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(path)-\(index).png"))
            }
            NSApp.terminate(nil)
        }
    }

    private func startupLocations() -> [Location] {
        if let launch = services.launchLocation { return [LocationInput.parseOrHome(launch)] }
        let preferences = services.preferences.preferences
        if preferences.restorePreviousTabs, let state = try? services.preferences.loadSession(), !state.locations.isEmpty { return state.locations }
        return [(try? Location(uri: preferences.newTabLocation)) ?? Location.home]
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { services.preferences.preferences.closeOnLastTab }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { windows.open((try? Location(uri: services.preferences.preferences.newTabLocation)) ?? Location.home) }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        if services.preferences.preferences.restorePreviousTabs {
            try? services.preferences.saveSession(Array(windows.locations.prefix(StateLimits.sessionTabs)))
        } else {
            try? services.preferences.clearSession()
        }
        services.operations.persistHistory()
        try? services.sidebarPreferences.save()
    }

    func application(_ sender: NSApplication, open urls: [URL]) {
        for url in urls where url.isFileURL {
            var isDirectory: ObjCBool = false
            let path = url.path
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) else { continue }
            let folder = isDirectory.boolValue ? path : (path as NSString).deletingLastPathComponent
            windows.open(Location(localPath: folder), asTabIn: windows.controllers.last?.window)
        }
    }

    // MARK: menu actions

    @objc func showPreferences(_ sender: Any?) {
        if preferencesWindow == nil { preferencesWindow = PreferencesWindowController(services: services) }
        preferencesWindow?.showWindow(nil)
        preferencesWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc func showAbout(_ sender: Any?) {
        NSApp.orderFrontStandardAboutPanel(options: [
            .credits: NSAttributedString(string: "A file manager for the Mac. Free software under the GNU General Public License, version 3 or later."),
        ])
    }

    @objc func showHelp(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = "Morin Explorer"
        alert.informativeText = "Browse with the sidebar or type a path into the location bar (⌘L). Switch views with ⌘1–⌘4, search with ⌘F, and open the operations list from the status bar."
        alert.runModal()
    }
}

extension LocationInput {
    static func parseOrHome(_ text: String) -> Location {
        let expanded = text.hasPrefix("~") ? NSHomeDirectory() + text.dropFirst() : text
        return (try? parse(expanded)) ?? Location.home
    }
}
