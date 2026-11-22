import XCTest
@testable import ExplorerDomain

final class InlineRenameTests: XCTestCase {
    private let interval: TimeInterval = 0.5

    private func click(_ trigger: inout InlineRenameTrigger, _ key: String = "a", only: Bool = true, count: Int = 1, at time: TimeInterval) -> Bool {
        trigger.clicked(key, onlySelection: only, clickCount: count, at: time, doubleClickInterval: interval)
    }

    func testASecondSlowClickOnTheSelectedItemStartsRenaming() {
        var trigger = InlineRenameTrigger()
        XCTAssertFalse(click(&trigger, only: false, at: 10))
        XCTAssertTrue(click(&trigger, at: 11))
    }

    func testTheFirstClickNeverStartsRenaming() {
        var trigger = InlineRenameTrigger()
        XCTAssertFalse(click(&trigger, only: true, at: 10))
    }

    func testAQuickSecondClickIsADoubleClickAndDoesNotRename() {
        var trigger = InlineRenameTrigger()
        _ = click(&trigger, only: false, at: 10)
        XCTAssertFalse(click(&trigger, count: 2, at: 10.2))
        XCTAssertFalse(click(&trigger, count: 1, at: 10.3))
    }

    func testHoweverLongTheGapTheSecondClickRenames() {
        var trigger = InlineRenameTrigger()
        _ = click(&trigger, only: false, at: 10)
        XCTAssertTrue(click(&trigger, at: 40))
    }

    func testAClickOnAnotherItemOrAMultiSelectionDoesNotRename() {
        var trigger = InlineRenameTrigger()
        _ = click(&trigger, "a", only: false, at: 10)
        XCTAssertFalse(click(&trigger, "b", at: 11))
        _ = click(&trigger, "a", only: false, at: 20)
        XCTAssertFalse(click(&trigger, "a", only: false, at: 21))
    }

    func testResettingForgetsTheLastClick() {
        var trigger = InlineRenameTrigger()
        _ = click(&trigger, only: false, at: 10)
        trigger.reset()
        XCTAssertFalse(click(&trigger, at: 11))
    }

    func testRenamingSelectsTheNameButNotTheExtension() {
        XCTAssertEqual(renameSelectionLength(for: "report.final.pdf", isDirectory: false), 12)
        XCTAssertEqual(renameSelectionLength(for: "Makefile", isDirectory: false), 8)
        XCTAssertEqual(renameSelectionLength(for: ".bashrc", isDirectory: false), 7)
        XCTAssertEqual(renameSelectionLength(for: "photos.2024", isDirectory: true), 11)
    }
}
