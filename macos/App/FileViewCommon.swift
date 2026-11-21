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

/// Turns an item's name label into a text field in place. Return or clicking away renames;
/// Escape leaves the name alone. The view that owns the label hears about the end through
/// `finished`, so it can take the keyboard back.
final class InlineRename: NSObject, NSTextFieldDelegate {
    private let field: NSTextField
    private let item: FileItem
    private let restoredName: String
    private let commit: (String) -> Void
    private let finished: () -> Void
    private var isFinished = false
    private var cancelled = false
    private let wasSelectable: Bool
    private let wasBezeled: Bool
    private let drewBackground: Bool

    init(field: NSTextField, item: FileItem, restoredName: String, commit: @escaping (String) -> Void, finished: @escaping () -> Void) {
        self.field = field
        self.item = item
        self.restoredName = restoredName
        self.commit = commit
        self.finished = finished
        wasSelectable = field.isSelectable
        wasBezeled = field.isBezeled
        drewBackground = field.drawsBackground
    }

    /// The part of a name label that holds text, in `view`'s coordinates: the label can be much wider
    /// than its text, and only a click on the text should rename.
    static func nameRect(of field: NSTextField, in view: NSView) -> NSRect {
        let size = field.cell?.cellSize(forBounds: field.bounds) ?? field.bounds.size
        let width = min(field.bounds.width, size.width + 4)
        let x: CGFloat
        switch field.alignment {
        case .center: x = (field.bounds.width - width) / 2
        case .right: x = field.bounds.width - width
        default: x = 0
        }
        return view.convert(NSRect(x: x, y: 0, width: width, height: field.bounds.height), from: field)
    }

    /// Whether the pointer is still where `event` pressed it, so a click is told apart from a drag.
    /// Call it once the superclass has finished tracking the mouse.
    static func pointerStayed(since event: NSEvent) -> Bool {
        guard let window = event.window else { return false }
        let start = window.convertPoint(toScreen: event.locationInWindow)
        let now = NSEvent.mouseLocation
        return hypot(now.x - start.x, now.y - start.y) < 4
    }

    func begin() {
        guard let window = field.window else { return end(commit: false) }
        field.isEditable = true
        field.isSelectable = true
        field.isBezeled = true
        field.bezelStyle = .squareBezel
        field.drawsBackground = true
        field.backgroundColor = .textBackgroundColor
        field.stringValue = item.name
        field.delegate = self
        window.makeFirstResponder(field)
        field.currentEditor()?.selectedRange = NSRange(location: 0, length: renameSelectionLength(for: item.name, isDirectory: item.isDirectory))
    }

    /// Drops the edit without renaming, for when the list under the field is about to change.
    func cancel() {
        guard !isFinished else { return }
        cancelled = true
        _ = field.abortEditing()
        end(commit: false)
    }

    func controlTextDidEndEditing(_ notification: Notification) { end(commit: !cancelled) }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        guard commandSelector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        cancelled = true
        field.window?.makeFirstResponder(nil)
        return true
    }

    private func end(commit shouldCommit: Bool) {
        guard !isFinished else { return }
        isFinished = true
        let name = field.stringValue.trimmingCharacters(in: .whitespaces)
        field.delegate = nil
        field.isEditable = false
        field.isSelectable = wasSelectable
        field.isBezeled = wasBezeled
        field.drawsBackground = drewBackground
        field.stringValue = restoredName
        finished()
        if shouldCommit, !name.isEmpty, name != item.name { commit(name) }
    }
}
