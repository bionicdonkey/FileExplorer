import Foundation

/// Reads where trashed items came from. Finder records that in the Trash folder's `.DS_Store`
/// (`ptbL` is the folder an item was deleted from, `ptbN` its name when it had to be changed), and
/// has no scripting command to act on it, so Put Back reads the record itself.
public enum TrashOrigins {
    /// Trashed name → the path it was deleted from, for a Trash whose items were deleted from under `volumeRoot`.
    public static func originalPaths(inDSStore data: Data, volumeRoot: String = "/") -> [String: String] {
        var parents: [String: String] = [:]
        var names: [String: String] = [:]
        for record in records(data) {
            guard let value = record.text else { continue }
            if record.id == "ptbL" { parents[record.name] = value } else if record.id == "ptbN" { names[record.name] = value }
        }
        var paths: [String: String] = [:]
        for (trashed, parent) in parents {
            let base = (volumeRoot as NSString).appendingPathComponent(parent)
            paths[trashed] = (base as NSString).appendingPathComponent(names[trashed] ?? trashed)
        }
        return paths
    }

    struct Record { var name: String; var id: String; var text: String? }

    private struct Reader {
        let bytes: [UInt8]
        var position: Int
        var failed = false
        init(_ bytes: [UInt8], at position: Int) { self.bytes = bytes; self.position = position }
        mutating func u32() -> UInt32 {
            guard position >= 0, position + 4 <= bytes.count else { failed = true; return 0 }
            defer { position += 4 }
            return bytes[position...position + 3].reduce(0) { $0 << 8 | UInt32($1) }
        }
        mutating func u8() -> UInt8 {
            guard position >= 0, position < bytes.count else { failed = true; return 0 }
            defer { position += 1 }
            return bytes[position]
        }
        mutating func skip(_ count: Int) { position += count }
        mutating func raw(_ count: Int) -> [UInt8] {
            guard count >= 0, position >= 0, position + count <= bytes.count else { failed = true; return [] }
            defer { position += count }
            return Array(bytes[position..<position + count])
        }
        mutating func utf16(_ units: Int) -> String {
            let data = raw(units * 2)
            return String(data: Data(data), encoding: .utf16BigEndian) ?? ""
        }
    }

    /// Every record of the file's B-tree, in order. A file that doesn't parse yields what was read so far.
    static func records(_ data: Data) -> [Record] {
        let bytes = [UInt8](data)
        guard bytes.count > 36, Array(bytes[4..<8]) == Array("Bud1".utf8) else { return [] }
        var header = Reader(bytes, at: 8)
        let rootOffset = Int(header.u32()) + 4
        var root = Reader(bytes, at: rootOffset)
        let count = Int(root.u32())
        root.skip(4)
        guard count < 1 << 20 else { return [] }
        var offsets: [UInt32] = []
        for _ in 0..<count { offsets.append(root.u32()) }
        root.skip((((count + 255) / 256) * 256 - count) * 4)
        let tableOfContents = Int(root.u32())
        var tree: Int?
        for _ in 0..<tableOfContents where !root.failed {
            let length = Int(root.u8())
            let name = String(decoding: root.raw(length), as: UTF8.self)
            let id = Int(root.u32())
            if name == "DSDB" { tree = id }
        }
        guard !root.failed, let tree, tree < offsets.count else { return [] }
        func position(of block: Int) -> Int? { block < offsets.count ? Int(offsets[block] & ~0x1f) + 4 : nil }
        guard let treePosition = position(of: tree) else { return [] }
        var info = Reader(bytes, at: treePosition)
        let rootNode = Int(info.u32())

        var found: [Record] = []
        var visited = Set<Int>()
        func record(_ reader: inout Reader) -> Bool {
            let name = reader.utf16(Int(reader.u32()))
            let id = String(decoding: reader.raw(4), as: UTF8.self)
            let type = String(decoding: reader.raw(4), as: UTF8.self)
            var text: String?
            switch type {
            case "bool": reader.skip(1)
            case "long", "shor", "type": reader.skip(4)
            case "comp", "dutc": reader.skip(8)
            case "ustr": text = reader.utf16(Int(reader.u32()))
            case "blob": reader.skip(Int(reader.u32()))
            default: return false
            }
            if reader.failed { return false }
            found.append(Record(name: name, id: id, text: text))
            return true
        }
        func walk(_ block: Int) -> Bool {
            guard !visited.contains(block), let at = position(of: block) else { return false }
            visited.insert(block)
            var reader = Reader(bytes, at: at)
            let rightmost = Int(reader.u32())
            let entries = Int(reader.u32())
            guard entries < 1 << 16 else { return false }
            if rightmost == 0 {
                for _ in 0..<entries { if !record(&reader) { return false } }
            } else {
                for _ in 0..<entries {
                    let child = Int(reader.u32())
                    if !walk(child) || !record(&reader) { return false }
                }
                return walk(rightmost)
            }
            return !reader.failed
        }
        _ = walk(rootNode)
        return found
    }
}
