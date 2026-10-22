import Foundation
import ExplorerDomain

/// A directory request held by the scripted port until a step answers it.
final class HeldDirectory {
    let request: DirectoryRequest
    let sink: DirectoryCallback
    init(request: DirectoryRequest, sink: @escaping DirectoryCallback) {
        self.request = request
        self.sink = sink
    }
}

/// Records directory requests; a request is answered only when a step says so.
final class ScriptedDirectory: DirectoryRepository {
    var held: [HeldDirectory] = []
    func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback) { held.append(HeldDirectory(request: request, sink: done)) }
    func listPages(_ request: DirectoryRequest, sink: @escaping DirectoryCallback) { held.append(HeldDirectory(request: request, sink: sink)) }
}

/// What a step's `as` name stands for.
enum Bound {
    case op(OperationID)
    case submit(Result<OperationID, OperationError>)
    case cancel(String)
    case location(Location)
}

/// Everything a domain scenario drives: one tab with its navigation and search coordinators, the
/// operation coordinator over a scripted backend, and the pure subjects' state. Every call happens
/// on the main thread, in step order, with no scheduler between a step and its assertion.
final class DomainWorld {
    let directory = ScriptedDirectory()
    let tab: TabState
    let navigation: NavigationCoordinator
    var bound: [String: Bound] = [:]
    let defaultCapabilities: JSON
    let operations = OperationsSubject()

    init(given: JSON) throws {
        let start = try Location(uri: given.string("location") ?? "file:///")
        tab = TabState(location: start)
        navigation = NavigationCoordinator(state: tab, repository: directory)
        defaultCapabilities = given.dict("capabilities") ?? [:]
    }
}

func location(_ uri: String) throws -> Location {
    do { return try Location(uri: uri) } catch { throw RunError("bad location \(uri.debugDescription)") }
}

func errorKind(_ name: String) throws -> OperationErrorKind {
    switch name {
    case "permission_denied": return .permissionDenied
    case "authentication_required": return .authenticationRequired
    case "mount_unavailable": return .mountUnavailable
    case "conflict": return .conflict
    case "not_found": return .notFound
    case "read_only": return .readOnly
    case "unsupported": return .unsupported
    case "capability_changed": return .capabilityChanged
    case "partial_completion": return .partialCompletion
    case "invalid_request": return .invalidRequest
    case "unknown": return .unknown
    default: throw RunError("unknown error kind \(name)")
    }
}

func errorKindName(_ kind: OperationErrorKind) -> String {
    switch kind {
    case .permissionDenied: return "permission_denied"
    case .authenticationRequired: return "authentication_required"
    case .mountUnavailable: return "mount_unavailable"
    case .conflict: return "conflict"
    case .notFound: return "not_found"
    case .readOnly: return "read_only"
    case .unsupported: return "unsupported"
    case .capabilityChanged: return "capability_changed"
    case .partialCompletion: return "partial_completion"
    case .invalidRequest: return "invalid_request"
    case .unknown: return "unknown"
    }
}

/// 1-based index into the requests received, or "latest".
func requestIndex(_ step: JSON, count: Int) throws -> Int {
    if let name = step.string("request"), name == "latest" {
        return try require(count > 0 ? count - 1 : nil, "no request received yet")
    }
    guard let n = step.int("request"), n >= 1, n <= count else {
        throw RunError("request \(step["request"] ?? "?") was never received (\(count) so far)")
    }
    return n - 1
}
