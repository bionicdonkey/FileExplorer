import AppKit
import ExplorerDomain

/// What a file view needs from the window around it. The views never touch domain state.
protocol FileViewHost: AnyObject {
    var tab: TabModel { get }
    func fileViewSelectionChanged(_ items: [FileItem])
    func fileViewActivate(_ item: FileItem)
    func fileViewVisible(_ items: [FileItem])
    func fileViewMenu(for items: [FileItem]) -> NSMenu
    func fileViewCanDrop(sources: [Location], onto destination: Location) -> Bool
    func fileViewDrop(sources: [Location], onto destination: Location, copy: Bool) -> Bool
    func fileViewRename(_ item: FileItem, to name: String)
}

/// The list and the grid implement this; the content controller swaps between them.
protocol FileView: AnyObject {
    var host: FileViewHost? { get set }
    var view: NSView { get }
    var focusView: NSView { get }
    func reload()
    func applySelection()
    func beginRename(_ item: FileItem)
    func scroll(to item: FileItem)
}

enum DropSupport {
    static func urls(from info: NSDraggingInfo) -> [URL] {
        (info.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
    }

    /// Option copies, command moves; otherwise a move on the same volume and a copy across volumes.
    static func operation(for urls: [URL], destination: Location, mask: NSDragOperation, flags: NSEvent.ModifierFlags) -> NSDragOperation {
        guard let path = destination.localPath, let first = urls.first else { return [] }
        if flags.contains(.option) { return .copy }
        if flags.contains(.command) { return .move }
        let source = try? first.resourceValues(forKeys: [.volumeIdentifierKey]).volumeIdentifier as? NSObject
        let target = try? URL(fileURLWithPath: path).resourceValues(forKeys: [.volumeIdentifierKey]).volumeIdentifier as? NSObject
        return (source != nil && source == target) ? .move : .copy
    }

    static func validate(_ info: NSDraggingInfo, host: FileViewHost, destination: Location) -> NSDragOperation {
        let urls = urls(from: info)
        let sources = urls.map { Location(localPath: $0.path) }
        guard !sources.isEmpty, host.fileViewCanDrop(sources: sources, onto: destination) else { return [] }
        let flags = NSEvent.modifierFlags
        let operation = operation(for: urls, destination: destination, mask: info.draggingSourceOperationMask, flags: flags)
        return info.draggingSourceOperationMask.contains(operation) ? operation : info.draggingSourceOperationMask.intersection([.copy, .move]).isEmpty ? [] : operation
    }

    static func accept(_ info: NSDraggingInfo, host: FileViewHost, destination: Location) -> Bool {
        let urls = urls(from: info)
        let sources = urls.map { Location(localPath: $0.path) }
        let operation = validate(info, host: host, destination: destination)
        guard !operation.isEmpty else { return false }
        return host.fileViewDrop(sources: sources, onto: destination, copy: operation == .copy)
    }
}

extension FileItem {
    /// A file URL for drag and drop and the pasteboard; nil for items that do not exist on disk (inside an archive).
    var pasteboardURL: NSURL? { location.localPath.map { URL(fileURLWithPath: $0) as NSURL } }
}

/// Asks for a new name for an item; used by both views so renaming behaves the same everywhere.
enum RenamePrompt {
    static func ask(for item: FileItem, in window: NSWindow?, completion: @escaping (String) -> Void) {
        let alert = NSAlert()
        alert.messageText = "Rename “\(item.name)”"
        alert.informativeText = "Enter a new name."
        alert.addButton(withTitle: "Rename")
        alert.addButton(withTitle: "Cancel")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 24))
        field.stringValue = item.name
        field.setAccessibilityLabel("New name")
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        let finish: (NSApplication.ModalResponse) -> Void = { response in
            let name = field.stringValue.trimmingCharacters(in: .whitespaces)
            if response == .alertFirstButtonReturn, !name.isEmpty, name != item.name { completion(name) }
        }
        if let window { alert.beginSheetModal(for: window, completionHandler: finish) } else { finish(alert.runModal()) }
        // Select the stem so typing replaces the name and keeps the extension.
        DispatchQueue.main.async {
            let stem = splitExtension(item.name).stem
            field.currentEditor()?.selectedRange = NSRange(location: 0, length: (stem as NSString).length)
        }
    }
}
