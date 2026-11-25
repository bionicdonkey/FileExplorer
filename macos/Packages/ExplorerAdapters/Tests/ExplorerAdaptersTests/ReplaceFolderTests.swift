import XCTest
import ExplorerDomain
@testable import ExplorerAdapters

final class ReplaceFolderTests: XCTestCase {
    func testReplacingAFolderMergesIntoItForCopyAndMove() throws {
        for kind in [MutationKind.copy, .move] {
            let root = NSTemporaryDirectory() + "replace-\(UUID().uuidString)"
            defer { try? FileManager.default.removeItem(atPath: root) }
            let source = root + "/a/folder", target = root + "/b/folder"
            let fm = FileManager.default
            try fm.createDirectory(atPath: source + "/sub", withIntermediateDirectories: true)
            try fm.createDirectory(atPath: target, withIntermediateDirectories: true)
            try "new".write(toFile: source + "/sub/new.txt", atomically: true, encoding: .utf8)
            try "new".write(toFile: source + "/same.txt", atomically: true, encoding: .utf8)
            try "old".write(toFile: target + "/same.txt", atomically: true, encoding: .utf8)
            try "keep".write(toFile: target + "/keep.txt", atomically: true, encoding: .utf8)
            var changed = false
            try FileExecutor.perform(kind, source, target, CancellationToken(), replace: true, changed: &changed) { _ in }
            XCTAssertEqual(try String(contentsOfFile: target + "/same.txt"), "new")
            XCTAssertEqual(try String(contentsOfFile: target + "/keep.txt"), "keep")
            XCTAssertEqual(try String(contentsOfFile: target + "/sub/new.txt"), "new")
            XCTAssertEqual(fm.fileExists(atPath: source), kind == .copy)
        }
    }
}
