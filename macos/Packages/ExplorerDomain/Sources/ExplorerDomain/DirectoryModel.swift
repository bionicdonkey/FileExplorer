import Foundation

public extension FileItem {
    /// Images and video, by MIME type or by the system type identifiers the macOS adapters report.
    var isVisualMedia: Bool {
        if contentType.hasPrefix("image/") || contentType.hasPrefix("video/") { return true }
        let visual = ["public.image", "public.png", "public.jpeg", "public.heic", "public.heif", "public.tiff", "public.gif", "com.compuserve.gif",
                      "public.webp", "com.adobe.raw-image", "public.movie", "public.video", "public.mpeg-4", "com.apple.quicktime-movie", "public.avi"]
        return visual.contains(contentType) || contentType.hasPrefix("public.camera-raw")
    }
}

/// Sorting, grouping and filtering of a directory's items, in a deterministic display order.
public enum DirectoryModel {
    public static func visible(_ items: [FileItem], options: ViewOptions) -> [FileItem] {
        sorted(options.showHidden ? items : items.filter { !$0.name.hasPrefix(".") }, options: options)
    }

    public static func sorted(_ items: [FileItem], options: ViewOptions) -> [FileItem] {
        items.sorted { left, right in
            if options.foldersFirst, left.isDirectory != right.isDirectory { return left.isDirectory }
            let order: ComparisonResult
            switch options.sortField {
            case .name: order = compareNames(left, right)
            case .dateModified: order = compare(left.modified, right.modified, then: compareNames(left, right))
            case .type: order = compare(left.contentType, right.contentType, then: compareNames(left, right))
            case .size: order = compare(left.size, right.size, then: compareNames(left, right))
            }
            return options.sortDirection == .ascending ? order == .orderedAscending : order == .orderedDescending
        }
    }

    private static func compare<T: Comparable>(_ a: T, _ b: T, then tie: ComparisonResult) -> ComparisonResult {
        a < b ? .orderedAscending : (a > b ? .orderedDescending : tie)
    }

    private static func compareNames(_ a: FileItem, _ b: FileItem) -> ComparisonResult {
        let (l, r) = (a.name.lowercased(), b.name.lowercased())
        if l != r { return l < r ? .orderedAscending : .orderedDescending }
        return a.name == b.name ? .orderedSame : (a.name < b.name ? .orderedAscending : .orderedDescending)
    }

    /// The heading an item sits under for a grouping, or nil when not grouped.
    public static func groupLabel(for item: FileItem, field: GroupField, now: Date = Date(), typeName: (FileItem) -> String) -> String? {
        switch field {
        case .none: return nil
        case .type: return item.isDirectory ? "Folders" : typeName(item)
        case .size:
            switch item.size {
            case 0: return "Empty"
            case 1...1_048_575: return "Small"
            case 1_048_576...104_857_599: return "Medium"
            case 104_857_600...1_073_741_823: return "Large"
            default: return "Very large"
            }
        case .dateModified: return dateGroup(item.modified, now: now)
        }
    }

    static func dateGroup(_ modified: Date, now: Date) -> String {
        let age = max(0, now.timeIntervalSince(modified))
        let day = 24.0 * 60 * 60
        switch age {
        case ..<day: return "Today"
        case ..<(2 * day): return "Yesterday"
        case ..<(7 * day): return "This week"
        case ..<(14 * day): return "Last week"
        case ..<(31 * day): return "Earlier this month"
        case ..<(366 * day): return "Earlier this year"
        default: return "Older"
        }
    }

    /// Items in display order with their group headings: consecutive items under one heading.
    public static func grouped(_ items: [FileItem], options: ViewOptions, now: Date = Date(), typeName: (FileItem) -> String) -> [(title: String?, items: [FileItem])] {
        guard options.groupField != .none else { return [(nil, items)] }
        var groups: [(title: String?, items: [FileItem])] = []
        for item in items {
            let title = groupLabel(for: item, field: options.groupField, now: now, typeName: typeName)
            if let index = groups.firstIndex(where: { $0.title == title }) { groups[index].items.append(item) } else { groups.append((title, [item])) }
        }
        return groups
    }
}
