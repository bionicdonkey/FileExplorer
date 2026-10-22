import AppKit

/// The menu bar. Items that act on the files have no target, so they reach whichever window is in
/// front through the responder chain; the window decides whether each one is available.
enum MainMenu {
    static func build(appName: String, delegate: AppDelegate) -> NSMenu {
        let main = NSMenu()
        func add(_ title: String, _ build: (NSMenu) -> Void) {
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let menu = NSMenu(title: title)
            menu.autoenablesItems = true
            build(menu)
            item.submenu = menu
            main.addItem(item)
        }
        let f2 = String(UnicodeScalar(NSF2FunctionKey)!)
        let backspace = String(UnicodeScalar(NSBackspaceCharacter)!)

        add(appName) { menu in
            menu.add("About \(appName)", #selector(AppDelegate.showAbout(_:)), target: delegate)
            menu.addItem(.separator())
            menu.add("Settings…", #selector(AppDelegate.showPreferences(_:)), key: ",", target: delegate)
            menu.addItem(.separator())
            let services = NSMenu(title: "Services")
            let servicesItem = NSMenuItem(title: "Services", action: nil, keyEquivalent: "")
            servicesItem.submenu = services
            menu.addItem(servicesItem)
            NSApp.servicesMenu = services
            menu.addItem(.separator())
            menu.add("Hide \(appName)", #selector(NSApplication.hide(_:)), key: "h")
            menu.add("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), key: "h", modifiers: [.command, .option])
            menu.add("Show All", #selector(NSApplication.unhideAllApplications(_:)))
            menu.addItem(.separator())
            menu.add("Quit \(appName)", #selector(NSApplication.terminate(_:)), key: "q")
        }
        add("File") { menu in
            menu.add("New Tab", #selector(ExplorerWindowController.newTab(_:)), key: "t")
            menu.add("New Window", #selector(ExplorerWindowController.newWindow(_:)), key: "n")
            menu.add("New Folder", #selector(ExplorerWindowController.newFolder(_:)), key: "n", modifiers: [.command, .shift])
            menu.addItem(.separator())
            menu.add("Open", #selector(ExplorerWindowController.openSelection(_:)), key: String(UnicodeScalar(NSDownArrowFunctionKey)!))
            menu.add("Open in New Tab", #selector(ExplorerWindowController.openInNewTabAction(_:)))
            menu.add("Open in New Window", #selector(ExplorerWindowController.openInNewWindowAction(_:)))
            menu.add("Open in Terminal", #selector(ExplorerWindowController.openInTerminal(_:)), key: "t", modifiers: [.command, .option])
            menu.addItem(.separator())
            menu.add("Get Info", #selector(ExplorerWindowController.getInfo(_:)), key: "i")
            menu.add("Rename…", #selector(ExplorerWindowController.renameItem(_:)), key: f2, modifiers: [])
            menu.add("Extract Here", #selector(ExplorerWindowController.extractHere(_:)))
            menu.add("Compress to .zip", #selector(ExplorerWindowController.compressZip(_:)))
            menu.add("Compress to .7z", #selector(ExplorerWindowController.compress7z(_:)))
            menu.add("Compress to .tar.gz", #selector(ExplorerWindowController.compressTarGz(_:)))
            menu.add("Add to Quick Access", #selector(ExplorerWindowController.togglePin(_:)), key: "d", modifiers: [.command, .control])
            menu.addItem(.separator())
            menu.add("Close Tab", #selector(NSWindow.performClose(_:)), key: "w")
            menu.add("Close Other Tabs", #selector(ExplorerWindowController.closeOtherTabs(_:)), key: "w", modifiers: [.command, .option])
            menu.add("Duplicate Tab", #selector(ExplorerWindowController.duplicateTab(_:)))
        }
        add("Edit") { menu in
            menu.add("Cut", #selector(ExplorerWindowController.cut(_:)), key: "x")
            menu.add("Copy", #selector(ExplorerWindowController.copy(_:)), key: "c")
            menu.add("Paste", #selector(ExplorerWindowController.paste(_:)), key: "v")
            menu.add("Paste into Folder", #selector(ExplorerWindowController.pasteIntoFolder(_:)), key: "v", modifiers: [.command, .option])
            menu.add("Select All", #selector(NSResponder.selectAll(_:)), key: "a")
            menu.addItem(.separator())
            menu.add("Copy Name", #selector(ExplorerWindowController.copyName(_:)))
            menu.add("Copy Path", #selector(ExplorerWindowController.copyPath(_:)), key: "c", modifiers: [.command, .option])
            menu.add("Copy To…", #selector(ExplorerWindowController.copyTo(_:)))
            menu.add("Move To…", #selector(ExplorerWindowController.moveTo(_:)))
            menu.addItem(.separator())
            menu.add("Move to Trash", #selector(ExplorerWindowController.moveToTrash(_:)), key: backspace)
            menu.add("Delete Immediately…", #selector(ExplorerWindowController.deletePermanently(_:)), key: backspace, modifiers: [.command, .option])
            menu.addItem(.separator())
            menu.add("Find", #selector(ExplorerWindowController.focusSearch(_:)), key: "f")
        }
        add("View") { menu in
            for (index, title) in ["as Detailed List", "as Compact List", "as Icon Grid", "as Hybrid"].enumerated() {
                menu.add(title, #selector(ExplorerWindowController.setViewMode(_:)), key: String(index + 1), tag: index)
            }
            menu.addItem(.separator())
            menu.add("Zoom In", #selector(ExplorerWindowController.zoomIn(_:)), key: "+")
            menu.add("Zoom Out", #selector(ExplorerWindowController.zoomOut(_:)), key: "-")
            menu.addItem(.separator())
            menu.add("Show Hidden Files", #selector(ExplorerWindowController.toggleHidden(_:)), key: ".", modifiers: [.command, .shift])
            menu.add("Show Thumbnails", #selector(ExplorerWindowController.toggleThumbnails(_:)))
            menu.add("List Folders First", #selector(ExplorerWindowController.toggleFoldersFirst(_:)))
            menu.addItem(.separator())
            let sort = NSMenuItem(title: "Sort By", action: nil, keyEquivalent: "")
            let sortMenu = NSMenu(title: "Sort By")
            for (index, title) in ["Name", "Date Modified", "Kind", "Size"].enumerated() { sortMenu.add(title, #selector(ExplorerWindowController.setSortField(_:)), tag: index) }
            sortMenu.addItem(.separator())
            sortMenu.add("Ascending", #selector(ExplorerWindowController.setSortDirection(_:)), tag: 0)
            sortMenu.add("Descending", #selector(ExplorerWindowController.setSortDirection(_:)), tag: 1)
            sort.submenu = sortMenu
            menu.addItem(sort)
            let group = NSMenuItem(title: "Group By", action: nil, keyEquivalent: "")
            let groupMenu = NSMenu(title: "Group By")
            for (index, title) in ["None", "Kind", "Size", "Date Modified"].enumerated() { groupMenu.add(title, #selector(ExplorerWindowController.setGroupField(_:)), tag: index) }
            group.submenu = groupMenu
            menu.addItem(group)
            menu.addItem(.separator())
            menu.add("Refresh", #selector(ExplorerWindowController.refresh(_:)), key: "r")
            menu.add("Reset This Folder’s View", #selector(ExplorerWindowController.resetFolderView(_:)))
            menu.add("Show Operations", #selector(ExplorerWindowController.showOperationsPage(_:)), key: "o", modifiers: [.command, .shift])
            menu.addItem(.separator())
            menu.add("Hide Sidebar", #selector(NSSplitViewController.toggleSidebar(_:)), key: "s", modifiers: [.command, .control])
            menu.add("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), key: "f", modifiers: [.command, .control])
        }
        add("Go") { menu in
            menu.add("Back", #selector(ExplorerWindowController.goBack(_:)), key: "[")
            menu.add("Forward", #selector(ExplorerWindowController.goForward(_:)), key: "]")
            menu.add("Enclosing Folder", #selector(ExplorerWindowController.goUp(_:)), key: String(UnicodeScalar(NSUpArrowFunctionKey)!))
            menu.add("Home", #selector(ExplorerWindowController.goHome(_:)), key: "h", modifiers: [.command, .shift])
            menu.addItem(.separator())
            menu.add("Go to Location…", #selector(ExplorerWindowController.focusLocation(_:)), key: "l")
        }
        add("Window") { menu in
            menu.add("Minimize", #selector(NSWindow.performMiniaturize(_:)), key: "m")
            menu.add("Zoom", #selector(NSWindow.performZoom(_:)))
            menu.addItem(.separator())
            menu.add("Show Previous Tab", #selector(NSWindow.selectPreviousTab(_:)), key: "\t", modifiers: [.control, .shift])
            menu.add("Show Next Tab", #selector(NSWindow.selectNextTab(_:)), key: "\t", modifiers: [.control])
            menu.add("Move Tab to New Window", #selector(NSWindow.moveTabToNewWindow(_:)))
            menu.add("Merge All Windows", #selector(NSWindow.mergeAllWindows(_:)))
            menu.addItem(.separator())
            menu.add("Bring All to Front", #selector(NSApplication.arrangeInFront(_:)))
            NSApp.windowsMenu = menu
        }
        add("Help") { menu in
            menu.add("\(appName) Help", #selector(AppDelegate.showHelp(_:)), target: delegate)
            NSApp.helpMenu = menu
        }
        return main
    }
}
