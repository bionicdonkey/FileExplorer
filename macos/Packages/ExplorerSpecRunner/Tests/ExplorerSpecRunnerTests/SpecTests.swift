import XCTest
@testable import ExplorerSpecRunner

final class SpecTests: XCTestCase {
    func specRoot() throws -> URL { try Spec.locate(from: URL(fileURLWithPath: #filePath)) }

    /// Every scenario file parses strictly, even while its feature is pending on macOS.
    func testSpecParses() throws {
        let spec = try Spec(root: try specRoot())
        XCTAssertGreaterThanOrEqual(spec.library.scenarios.count, 90)
        XCTAssertEqual(spec.registry.features.count, 25)
        XCTAssertTrue(spec.vocabulary.actions.contains("operate"))
    }

    /// Every scenario the registry makes non-pending for macOS runs, and passes.
    func testSelectedScenariosPass() throws {
        let spec = try Spec(root: try specRoot())
        var lines: [String] = []
        let failures = runSelected(spec: spec, filters: [], repeatCount: 1) { lines.append($0) }
        XCTAssertEqual(failures, 0, lines.joined(separator: "\n"))
    }

    func testBrokenScenarioIsRejected() throws {
        let root = try specRoot()
        let copy = FileManager.default.temporaryDirectory.appendingPathComponent("spec-copy-\(UUID().uuidString)")
        try FileManager.default.copyItem(at: root, to: copy)
        defer { try? FileManager.default.removeItem(at: copy) }
        let victim = copy.appendingPathComponent("scenarios/domain/navigation/latest-request-wins.json")
        try "{ not json".write(to: victim, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try Spec(root: copy))
        let unknown = try String(contentsOf: root.appendingPathComponent("scenarios/domain/navigation/latest-request-wins.json"))
            .replacingOccurrences(of: "\"do\": \"navigate\"", with: "\"do\": \"teleport\"")
        try unknown.write(to: victim, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try Spec(root: copy)) { XCTAssertTrue("\($0)".contains("unknown action"), "\($0)") }
    }
}
