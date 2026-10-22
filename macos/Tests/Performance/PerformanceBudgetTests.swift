import XCTest
import ExplorerAdapters
import ExplorerDomain
@testable import MorinExplorer

/// p95 budgets, ported from the scenarios in the Linux app's `tests/performance.rs`. The budgets are
/// per platform (spec section 5.6); these start from the same numbers and the measured values on the
/// reference Mac are recorded in `README.md`.
final class PerformanceBudgetTests: XCTestCase {
    private var directory: URL!

    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("perf-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() { try? FileManager.default.removeItem(at: directory) }

    private func p95(_ samples: [TimeInterval]) -> TimeInterval {
        let sorted = samples.sorted()
        return sorted[min(sorted.count - 1, Int((Double(sorted.count) * 0.95).rounded(.up)) - 1)]
    }

    private func assertP95(_ label: String, _ samples: [TimeInterval], under budget: TimeInterval, file: StaticString = #filePath, line: UInt = #line) {
        let value = p95(samples)
        print("budget \(label): p95 \(Int(value * 1000)) ms (budget \(Int(budget * 1000)) ms)")
        XCTAssertLessThanOrEqual(value, budget, "\(label) p95 \(value * 1000) ms exceeds \(budget * 1000) ms", file: file, line: line)
    }

    func testLocalTenThousandItemsFirstContentAndAcknowledgementMeetTheirBudgets() {
        for n in 0..<10_000 { FileManager.default.createFile(atPath: directory.appendingPathComponent("file-\(String(format: "%05d", n))").path, contents: Data("fixture".utf8)) }
        let root = Location(localPath: directory.path)
        var firstContent: [TimeInterval] = []
        var acknowledgement: [TimeInterval] = []
        for run in 0..<16 {
            let state = TabState(location: root)
            let navigation = NavigationCoordinator(state: state, repository: DirectoryRouter())
            var first: TimeInterval?
            let started = Date()
            navigation.navigate(to: root) { accepted in if accepted && first == nil { first = Date().timeIntervalSince(started) } }
            let acknowledged = Date().timeIntervalSince(started)
            let deadline = Date().addingTimeInterval(10)
            while state.hasPendingNavigation && Date() < deadline { RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.001)) }
            XCTAssertEqual(state.directory?.entries.count, 10_000)
            if run > 0, let first { firstContent.append(first); acknowledgement.append(acknowledged) }
        }
        assertP95("local first content", firstContent, under: 0.5)
        assertP95("navigation acknowledgement", acknowledgement, under: 0.1)
    }

    func testHundredHistoryRecordsLoadWithinTheirBudgets() throws {
        let records = (1...100).map { id -> [String: Any] in
            ["id": id, "kind": "copy", "sources": ["file:///fixture/a"], "completed": 1, "total": 1, "state": "Completed", "finished_at": Int(Date().timeIntervalSince1970)]
        }
        let file = directory.appendingPathComponent("history.json")
        try JSONSerialization.data(withJSONObject: ["schema_version": OperationManager.schemaVersion, "records": records]).write(to: file)
        var cold: [TimeInterval] = []
        var warm: [TimeInterval] = []
        for run in 0..<31 {
            let started = Date()
            let manager = OperationManager(backend: LocalFileOperations(), capabilities: FileExecutor.localCapabilities, historyFile: file, retentionDays: 7)
            let loaded = Date().timeIntervalSince(started)
            XCTAssertEqual(manager.records.count, 100)
            let projected = Date()
            XCTAssertEqual(manager.activeCount, 0)
            if run > 0 { cold.append(loaded); warm.append(Date().timeIntervalSince(projected)) }
        }
        assertP95("100-record cold history load", cold, under: 0.1)
        assertP95("100-record warm history projection", warm, under: 0.1)
    }

    func testSortingAndGroupingFiftyThousandItemsIsFast() throws {
        let entries = (0..<50_000).map { n -> FileItem in
            FileItem(snapshot: EntrySnapshot(location: Location(localPath: "/fixture/file-\(n)"), displayName: "file-\((n * 7919) % 50_000)", kind: .file, size: UInt64(n),
                                             modified: Date(timeIntervalSince1970: TimeInterval(n)), contentType: "public.plain-text", capabilities: CapabilitySnapshot()))
        }
        var options = ViewOptions()
        options.sortField = .size
        options.groupField = .size
        var samples: [TimeInterval] = []
        for run in 0..<11 {
            let started = Date()
            let groups = DirectoryModel.grouped(DirectoryModel.visible(entries, options: options), options: options, typeName: { _ in "Text" })
            XCTAssertFalse(groups.isEmpty)
            if run > 0 { samples.append(Date().timeIntervalSince(started)) }
        }
        assertP95("sort and group 50,000 items", samples, under: 1.0)
    }

    func testSessionAndHistoryStayBounded() throws {
        let many = (0..<500).map { Location(localPath: "/tab/\($0)") }
        let data = try SessionCodec.encode(many)
        XCTAssertEqual(try SessionCodec.decode(data).count, StateLimits.sessionTabs)
        var history = NavigationHistory()
        for n in 0..<1_000 { history.push(Location(localPath: "/h/\(n)")) }
        var steps = 0
        while history.back() != nil { steps += 1 }
        XCTAssertLessThan(steps, NavigationHistory.maximumRecords)
    }
}
