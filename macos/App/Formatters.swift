import AppKit
import UniformTypeIdentifiers
import ExplorerAdapters
import ExplorerDomain

enum Formatters {
    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("d MMM yyyy jj:mm")
        return formatter
    }()

    static func date(_ date: Date) -> String { date.timeIntervalSince1970 <= 0 ? "—" : dateFormatter.string(from: date) }
    static func size(_ item: FileItem) -> String { item.isDirectory ? "—" : ByteFormat.string(item.size) }
    static func type(_ item: FileItem) -> String {
        // A file with no recognisable type is reported by the system as plain "data".
        if !item.isDirectory, ["public.data", "application/octet-stream"].contains(item.contentType) { return "Document" }
        return OpenService.typeDescription(forContentType: item.contentType, isDirectory: item.isDirectory)
    }
}

enum Symbol {
    static func image(_ name: String, description: String? = nil) -> NSImage? {
        NSImage(systemSymbolName: name, accessibilityDescription: description) ?? NSImage(systemSymbolName: "questionmark.square", accessibilityDescription: description)
    }
}

/// System icons by content type, shared by the list, the grid and the sidebar.
final class IconCache {
    static let shared = IconCache()
    private var cache: [String: NSImage] = [:]

    func icon(for item: FileItem, points: Int) -> NSImage {
        let ext = (item.name as NSString).pathExtension.lowercased()
        let kind = item.isDirectory ? (ext == "app" ? "app" : "dir") : (item.contentTypeResolved ? item.contentType : "ext:" + ext)
        let key = "\(kind)@\(points)"
        if let hit = cache[key] { return hit }
        let type: UTType
        if item.isDirectory { type = ext == "app" ? .applicationBundle : .folder }
        else if item.contentTypeResolved, let known = UTType(item.contentType) { type = known }
        else { type = UTType(filenameExtension: ext) ?? .data }
        let image = (NSWorkspace.shared.icon(for: type).copy() as? NSImage) ?? NSImage()
        image.size = NSSize(width: points, height: points)
        cache[key] = image
        return image
    }
}

extension NSMenu {
    @discardableResult
    func add(_ title: String, _ action: Selector?, key: String = "", modifiers: NSEvent.ModifierFlags = [.command], target: AnyObject? = nil,
             enabled: Bool = true, reason: String? = nil, tag: Int = 0) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: enabled ? action : nil, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        item.target = target
        item.tag = tag
        if !enabled, let reason { item.toolTip = reason }
        addItem(item)
        return item
    }
}
