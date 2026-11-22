import Foundation

/// Decides when a click on an item's name should start renaming it in place: the click that
/// selects the item, then a second, slow one. Two quick clicks are a double click, which opens
/// the item, so they never rename. However long the pause, a click on the name of an item you just
/// clicked is a rename.
public struct InlineRenameTrigger {
    private var last: (key: String, time: TimeInterval)?

    public init() {}

    /// Records a click on the name of the item with identity `key`, and says whether it starts a rename.
    /// - Parameters:
    ///   - onlySelection: the item was the one and only selected item when the click began.
    ///   - doubleClickInterval: the system's double-click time; a second click inside it is a double click.
    public mutating func clicked(_ key: String, onlySelection: Bool, clickCount: Int, at time: TimeInterval, doubleClickInterval: TimeInterval) -> Bool {
        let previous = last
        last = (key, time)
        guard clickCount == 1, onlySelection, let previous, previous.key == key else { return false }
        return time - previous.time > doubleClickInterval
    }

    public mutating func reset() { last = nil }
}

/// Where the selection in a name being renamed ends: before the extension for a file, so typing
/// replaces the name and keeps the extension; at the end for a folder or a name with no extension.
/// Counted in UTF-16 units, which is what text fields select by.
public func renameSelectionLength(for name: String, isDirectory: Bool) -> Int {
    (isDirectory ? name : splitExtension(name).stem).utf16.count
}
