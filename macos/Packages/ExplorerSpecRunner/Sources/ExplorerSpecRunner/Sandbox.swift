import Darwin
import Foundation
import ExplorerAdapters

/// The temporary directory an adapter scenario runs in, built from `given.tree`.
final class Sandbox {
    let root: String
    private var restricted: [String] = []
    private let specRoot: URL

    init(specRoot: URL) throws {
        self.specRoot = specRoot
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("spec-sandbox-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        // Resolved, so paths compare equal to what realpath reports (/var is /private/var).
        root = base.resolvingSymlinksInPath().path
    }

    deinit {
        // Restore permissions so the directory can be deleted.
        for path in restricted { chmod(path, PathInfo.at(path) == .directory ? 0o755 : 0o644) }
        try? FileManager.default.removeItem(atPath: root)
    }

    func path(_ relative: String) -> String { relative.isEmpty ? root : (root as NSString).appendingPathComponent(relative) }

    func build(_ tree: Any?) throws {
        for case let entry as JSON in (tree as? [Any]) ?? [] { try buildEntry(entry) }
    }

    private func buildEntry(_ entry: JSON) throws {
        let fm = FileManager.default
        func parent(of path: String) throws { try fm.createDirectory(atPath: (path as NSString).deletingLastPathComponent, withIntermediateDirectories: true) }
        if let dir = entry.string("dir") {
            try fm.createDirectory(atPath: path(dir), withIntermediateDirectories: true)
            try restrict(path(dir), entry.string("mode"))
        } else if let file = entry.string("file") {
            let target = path(file)
            try parent(of: target)
            let data: Data
            if let content = entry.string("content") { data = Data(content.utf8) } else { data = Data(repeating: UInt8(ascii: "x"), count: entry.int("bytes") ?? 0) }
            try data.write(to: URL(fileURLWithPath: target))
            try restrict(target, entry.string("mode"))
        } else if let fixture = entry.string("copy_fixture") {
            let to = path(try require(entry.string("to"), "copy_fixture needs `to`"))
            try parent(of: to)
            try fm.copyItem(at: specRoot.appendingPathComponent("fixtures").appendingPathComponent(fixture), to: URL(fileURLWithPath: to))
        } else if let link = entry.string("symlink") {
            let target = path(try require(entry.string("target"), "symlink needs `target`"))
            try parent(of: path(link))
            try fm.createSymbolicLink(atPath: path(link), withDestinationPath: target)
        } else if let dir = entry.string("generate") {
            let count = try require(entry.int("count"), "generate needs `count`")
            let pattern = try require(entry.string("pattern"), "generate needs `pattern`")
            try fm.createDirectory(atPath: path(dir), withIntermediateDirectories: true)
            for n in 0..<count { fm.createFile(atPath: path(dir + "/" + Sandbox.expand(pattern, n)), contents: Data()) }
        } else {
            throw RunError("unknown tree entry \(entry)")
        }
    }

    private func restrict(_ path: String, _ mode: String?) throws {
        guard let mode else { return }
        switch mode {
        case "readonly": chmod(path, 0o555)
        case "unreadable": chmod(path, 0)
        default: throw RunError("unknown mode \(mode)")
        }
        restricted.append(path)
    }

    /// `file-{n:05}.txt` becomes `file-00007.txt`.
    static func expand(_ pattern: String, _ n: Int) -> String {
        guard let open = pattern.range(of: "{n"), let close = pattern.range(of: "}", range: open.upperBound..<pattern.endIndex) else { return pattern }
        let spec = pattern[open.upperBound..<close.lowerBound].trimmingCharacters(in: CharacterSet(charactersIn: ":0"))
        let width = Int(spec) ?? 0
        let digits = String(n)
        let padded = String(repeating: "0", count: max(0, width - digits.count)) + digits
        return String(pattern[..<open.lowerBound]) + padded + String(pattern[close.upperBound...])
    }

    func remove(_ relative: String) throws { try FileManager.default.removeItem(atPath: path(relative)) }

    /// True when this process can read a mode-000 file, so it cannot see permission failures.
    var isPrivileged: Bool { geteuid() == 0 }
}
