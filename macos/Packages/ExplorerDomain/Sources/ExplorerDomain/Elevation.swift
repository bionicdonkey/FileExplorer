import Foundation

/// A single operation re-run with elevated privileges. The app is never elevated: a privileged
/// helper performs exactly one validated operation per request, then goes back to idle.
public struct ElevatedRequest: Equatable {
    public enum Verb: String, CaseIterable { case create, copy, move, trash, delete }

    public var verb: Verb
    public var source: String
    public var destination: String?

    public init(verb: Verb, source: String, destination: String? = nil) {
        self.verb = verb
        self.source = source
        self.destination = destination
    }

    /// Local, absolute filesystem paths only. A network mount that the system projects into the
    /// filesystem is not the user's own, so it must never reach the helper.
    public static func isLocalPath(_ path: String) -> Bool {
        guard path.hasPrefix("/"), !path.contains("\0") else { return false }
        let remoteRoots = ["/Volumes/.timemachine", "/net", "/Network"]
        if remoteRoots.contains(where: { path == $0 || path.hasPrefix($0 + "/") }) { return false }
        return !path.split(separator: "/").contains("gvfs")
    }

    /// The reason this request is refused, or nil when it is acceptable. The helper calls this
    /// again itself: it does not trust the app that sent the request.
    public var violation: String? {
        guard ElevatedRequest.isLocalPath(source), destination.map(ElevatedRequest.isLocalPath) ?? true else {
            return "elevated retry only supports local paths"
        }
        switch verb {
        case .copy, .move: return destination == nil ? "\(verb.rawValue) requires a destination" : nil
        case .create, .trash, .delete: return destination != nil ? "\(verb.rawValue) takes no destination" : nil
        }
    }

    /// The request for retrying a failed operation: a single local source, never a batch, and never
    /// anything but a plain file mutation.
    public init?(retrying request: OperationRequest) {
        guard request.sources.count == 1,
              let source = request.sources[0].localPath,
              (request.destination ?? request.sources[0]).localPath != nil,
              (request.explicitTarget ?? request.sources[0]).localPath != nil else { return nil }
        let verb: Verb
        switch request.kind {
        case .copy: verb = .copy
        case .move, .rename: verb = .move
        case .trash: verb = .trash
        case .delete: verb = .delete
        case .create: verb = .create
        }
        var destination: String?
        if verb == .copy || verb == .move {
            if let target = request.explicitTarget?.localPath { destination = target }
            else if let folder = request.destination?.localPath { destination = (folder as NSString).appendingPathComponent((source as NSString).lastPathComponent) }
            else { return nil }
        }
        self.init(verb: verb, source: source, destination: destination)
        if violation != nil { return nil }
    }
}

public extension OperationManager {
    /// A failed record is eligible for a privileged retry only when it is one local operation that
    /// failed with permission denied (and so changed nothing). Batches, archives and anything outside
    /// the user's own filesystem are refused.
    func isEligibleForElevatedRetry(_ id: UInt64) -> Bool {
        guard let record = records.first(where: { $0.id == id }), case .failed(let key) = record.state,
              key == OperationError(.permissionDenied).messageKey, summary(id)?.effect == "unchanged" || summary(id) == nil,
              record.sourceURIs.count == 1, ![.compress, .extract].contains(record.kind) else { return false }
        return (record.sourceURIs + [record.destinationURI].compactMap { $0 }).allSatisfy { uri in
            uri.hasPrefix("file:") && ((try? Location(uri: uri))?.localPath.map(ElevatedRequest.isLocalPath) ?? false)
        }
    }
}

public extension OperationManager {
    /// The single privileged request that would redo a failed record, if it is eligible.
    func elevatedRequest(for id: UInt64) -> ElevatedRequest? {
        guard isEligibleForElevatedRetry(id), let record = records.first(where: { $0.id == id }), let mutation = record.kind.mutation,
              let source = try? Location(uri: record.sourceURIs[0]) else { return nil }
        let request = OperationRequest(id: OperationID(id), kind: mutation, sources: [source],
                                       destination: record.destinationURI.flatMap { try? Location(uri: $0) },
                                       explicitTarget: summary(id)?.explicitTarget.flatMap { try? Location(uri: $0) },
                                       capabilityVersion: 0, cancellation: CancellationToken())
        return ElevatedRequest(retrying: request)
    }
}
