import Foundation

public enum ViewMode: String, Codable, CaseIterable, Equatable {
    case detailedList = "detailed-list", compactList = "compact-list", iconGrid = "icon-grid", hybrid
}

public enum GridSize: String, Codable, CaseIterable, Equatable {
    case small, medium, large, extraLarge = "extra-large"

    public var iconSize: Int {
        switch self { case .small: return 48; case .medium: return 80; case .large: return 128; case .extraLarge: return 192 }
    }

    public var listIconSize: Int {
        switch self { case .small: return 16; case .medium: return 20; case .large: return 28; case .extraLarge: return 36 }
    }

    public var rowSpacing: Int {
        switch self { case .small: return 2; case .medium: return 6; case .large: return 10; case .extraLarge: return 14 }
    }

    public struct Layout: Equatable {
        public var tileWidth: Int
        public var verticalPadding: Int
        public var labelWidthCharacters: Int
        public var gap: Int
    }

    public var layout: Layout {
        switch self {
        case .small: return Layout(tileWidth: 128, verticalPadding: 4, labelWidthCharacters: 14, gap: 8)
        case .medium: return Layout(tileWidth: 172, verticalPadding: 6, labelWidthCharacters: 18, gap: 12)
        case .large: return Layout(tileWidth: 232, verticalPadding: 8, labelWidthCharacters: 24, gap: 16)
        case .extraLarge: return Layout(tileWidth: 300, verticalPadding: 10, labelWidthCharacters: 30, gap: 20)
        }
    }
}

public enum SortField: String, Codable, CaseIterable, Equatable { case name, dateModified = "date-modified", type, size }
public enum SortDirection: String, Codable, Equatable { case ascending, descending }
public enum GroupField: String, Codable, CaseIterable, Equatable { case none, type, size, dateModified = "date-modified" }

public enum ListColumn: String, Codable, CaseIterable, Comparable, Equatable {
    case name, dateModified = "date-modified", type, size

    public static func < (lhs: ListColumn, rhs: ListColumn) -> Bool { all.firstIndex(of: lhs)! < all.firstIndex(of: rhs)! }
    public static var all: [ListColumn] { [.name, .dateModified, .type, .size] }

    public var defaultWidth: Int {
        switch self { case .name: return 360; case .dateModified: return 150; case .type: return 160; case .size: return 100 }
    }
}

public struct ListColumnLayout: Codable, Equatable {
    public var order: [ListColumn] = ListColumn.all
    public var widths: [ListColumn: Int] = [:]

    public init(order: [ListColumn] = ListColumn.all, widths: [ListColumn: Int] = [:]) {
        self.order = order
        self.widths = widths
    }

    /// Every column exactly once, in the stored order then the default order, with sane widths.
    public func normalized() -> ListColumnLayout {
        var seen: [ListColumn] = []
        for column in order where !seen.contains(column) { seen.append(column) }
        for column in ListColumn.all where !seen.contains(column) { seen.append(column) }
        return ListColumnLayout(order: seen, widths: widths.mapValues { min(max($0, 64), 900) })
    }

    public func width(for column: ListColumn) -> Int { widths[column] ?? column.defaultWidth }

    enum CodingKeys: String, CodingKey { case order, widths }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        order = try container.decodeIfPresent([ListColumn].self, forKey: .order) ?? ListColumn.all
        let raw = try container.decodeIfPresent([String: Int].self, forKey: .widths) ?? [:]
        widths = Dictionary(uniqueKeysWithValues: raw.compactMap { key, value in ListColumn(rawValue: key).map { ($0, value) } })
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(order, forKey: .order)
        try container.encode(Dictionary(uniqueKeysWithValues: widths.map { ($0.key.rawValue, $0.value) }), forKey: .widths)
    }
}

public struct ViewOptions: Codable, Equatable {
    public var viewMode = ViewMode.hybrid
    public var gridSize = GridSize.medium
    public var sortField = SortField.name
    public var sortDirection = SortDirection.ascending
    public var foldersFirst = true
    public var groupField = GroupField.none
    public var thumbnails = true
    public var showHidden = false
    public var listColumns = ListColumnLayout()

    public init() {}

    enum CodingKeys: String, CodingKey {
        case viewMode = "view_mode", gridSize = "grid_size", sortField = "sort_field", sortDirection = "sort_direction"
        case foldersFirst = "folders_first", groupField = "group_field", thumbnails, showHidden = "show_hidden", listColumns = "list_columns"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = ViewOptions()
        viewMode = try c.decodeIfPresent(ViewMode.self, forKey: .viewMode) ?? d.viewMode
        gridSize = try c.decodeIfPresent(GridSize.self, forKey: .gridSize) ?? d.gridSize
        sortField = try c.decodeIfPresent(SortField.self, forKey: .sortField) ?? d.sortField
        sortDirection = try c.decodeIfPresent(SortDirection.self, forKey: .sortDirection) ?? d.sortDirection
        foldersFirst = try c.decodeIfPresent(Bool.self, forKey: .foldersFirst) ?? d.foldersFirst
        groupField = try c.decodeIfPresent(GroupField.self, forKey: .groupField) ?? d.groupField
        thumbnails = try c.decodeIfPresent(Bool.self, forKey: .thumbnails) ?? d.thumbnails
        showHidden = try c.decodeIfPresent(Bool.self, forKey: .showHidden) ?? d.showHidden
        listColumns = try c.decodeIfPresent(ListColumnLayout.self, forKey: .listColumns) ?? d.listColumns
    }
}

/// View options per folder, with global defaults beneath them, and an automatic choice (a grid for
/// a pictures or videos folder, or one that is mostly images and video) beneath those.
public final class ViewPreferencesStore {
    private struct Stored: Codable {
        var defaults: ViewOptions?
        var folders: [String: ViewOptions] = [:]
    }

    private let file: URL?
    private var defaults: ViewOptions?
    private var folders: [String: ViewOptions] = [:]

    public static func inMemory() -> ViewPreferencesStore { ViewPreferencesStore(file: nil) }

    public init(file: URL?) {
        self.file = file
        if let file, let data = try? BoundedState.read(file), let stored = try? JSONDecoder().decode(Stored.self, from: data) {
            defaults = stored.defaults
            folders = stored.folders
        }
    }

    private func key(_ path: String) -> String { (path as NSString).resolvingSymlinksInPath }

    public func options(for path: String, items: [FileItem] = []) -> ViewOptions {
        folders[key(path)] ?? defaults ?? ViewPreferencesStore.automatic(path: path, items: items)
    }

    public var globalDefaults: ViewOptions { defaults ?? ViewOptions() }

    public func setDefaults(_ options: ViewOptions) {
        var normalized = options
        normalized.listColumns = options.listColumns.normalized()
        defaults = normalized
    }

    @discardableResult public func clear(_ path: String) -> Bool { folders.removeValue(forKey: key(path)) != nil }

    @discardableResult
    public func clearAllFolders() -> Int {
        defer { folders.removeAll() }
        return folders.count
    }

    public func set(_ options: ViewOptions, for path: String) {
        var normalized = options
        normalized.listColumns = options.listColumns.normalized()
        folders[key(path)] = normalized
    }

    public func save() throws {
        guard let file else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try BoundedState.write(try encoder.encode(Stored(defaults: defaults, folders: folders)), to: file)
    }

    static func automatic(path: String, items: [FileItem]) -> ViewOptions {
        let name = (path as NSString).lastPathComponent.lowercased()
        let visualFolder = name == "pictures" || name == "videos" || name == "movies"
        let visualMajority = !items.isEmpty && items.filter(\.isVisualMedia).count * 2 > items.count
        var options = ViewOptions()
        if visualFolder || visualMajority { options.viewMode = .iconGrid }
        return options
    }
}
