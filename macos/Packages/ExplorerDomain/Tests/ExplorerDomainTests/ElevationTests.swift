import XCTest
@testable import ExplorerDomain

final class ElevationTests: XCTestCase {
    private func request(_ kind: MutationKind, sources: [String] = ["/tmp/a"], destination: String? = nil, target: String? = nil) -> OperationRequest {
        OperationRequest(id: OperationID(1), kind: kind, sources: sources.map { try! Location(uri: $0.contains("://") ? $0 : "file://" + $0) },
                         destination: destination.map { try! Location(uri: $0.contains("://") ? $0 : "file://" + $0) },
                         explicitTarget: target.map { try! Location(uri: "file://" + $0) })
    }

    func testASingleLocalCopyBecomesACopyIntoTheFolder() {
        let elevated = ElevatedRequest(retrying: request(.copy, destination: "/var/data"))
        XCTAssertEqual(elevated, ElevatedRequest(verb: .copy, source: "/tmp/a", destination: "/var/data/a"))
    }

    func testRenameIsAMoveToTheExplicitTarget() {
        XCTAssertEqual(ElevatedRequest(retrying: request(.rename, target: "/tmp/b"))?.verb, .move)
    }

    func testBatchesAndRemoteLocationsAreNeverRetried() {
        XCTAssertNil(ElevatedRequest(retrying: request(.copy, sources: ["/tmp/a", "/tmp/b"], destination: "/var")))
        XCTAssertNil(ElevatedRequest(retrying: request(.trash, sources: ["smb://host/share/a"])))
    }

    func testTheHelperRefusesWhatThePolicyRefuses() {
        XCTAssertNotNil(ElevatedRequest(verb: .copy, source: "/tmp/a").violation)
        XCTAssertNotNil(ElevatedRequest(verb: .delete, source: "/tmp/a", destination: "/x").violation)
        XCTAssertNotNil(ElevatedRequest(verb: .delete, source: "relative/path").violation)
        XCTAssertNotNil(ElevatedRequest(verb: .delete, source: "/net/server/file").violation)
        XCTAssertNil(ElevatedRequest(verb: .move, source: "/tmp/a", destination: "/tmp/b").violation)
    }

    func testEligibilityNeedsASingleLocalPermissionFailure() {
        let manager = OperationManager(backend: NeverBackend(), capabilities: CapabilitySnapshot(version: 1, supported: [.copy, .move, .create, .trash, .delete, .rename]))
        let id = manager.submit(.delete, sources: [Location(localPath: "/tmp/a")])   // unconfirmed: fails as invalid_request
        XCTAssertFalse(manager.isEligibleForElevatedRetry(id))
    }
}

private final class NeverBackend: FileOperations {
    func inspectCapabilities(_ request: CapabilityRequest, done: @escaping CapabilityCallback) {}
    func submit(_ request: OperationRequest, sink: @escaping OperationSink) {}
}
