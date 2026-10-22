import Foundation

// The three ports the domain depends on. Completion-handler APIs, never blocking: a call returns
// promptly and its answer arrives later, on the caller's own thread (the main thread), never
// before the call returns. Each one-shot callback is consumed exactly once, including on
// cancellation or failure. Operations emit ordered events with exactly one terminal event.
// Cancellation is intent only. See spec/contracts/ports.md.
//
// The domain is single-threaded by contract: every call into it, and every callback out of an
// adapter, happens on the main thread. Adapters do their work elsewhere and hop back.

public typealias DirectoryCallback = (RequestCompletion<DirectorySnapshot>) -> Void
public typealias VisibleMetadataCallback = (RequestCompletion<[VisibleMetadata]>) -> Void
public typealias CapabilityCallback = (RequestCompletion<CapabilitySnapshot>) -> Void
public typealias OperationSink = (OperationEvent) -> Void
public typealias SearchCallback = (SearchCompletion) -> Void

/// Stops a watch when released or cancelled.
public protocol WatchHandle: AnyObject {
    func cancel()
}

public protocol DirectoryRepository: AnyObject {
    func list(_ request: DirectoryRequest, done: @escaping DirectoryCallback)
    /// Ordered pages from one cursor, ending in exactly one page with no continuation, or a
    /// failure or cancellation. The default answers with a single page.
    func listPages(_ request: DirectoryRequest, sink: @escaping DirectoryCallback)
    /// Advisory metadata for rows that are currently visible. Never delays a listing.
    func inspectVisibleMetadata(_ request: VisibleMetadataRequest, done: @escaping VisibleMetadataCallback)
    /// Watches this directory only, never a tree.
    func watch(_ location: Location, changed: @escaping () -> Void) -> WatchHandle?
}

public extension DirectoryRepository {
    func listPages(_ request: DirectoryRequest, sink: @escaping DirectoryCallback) { list(request, done: sink) }

    func inspectVisibleMetadata(_ request: VisibleMetadataRequest, done: @escaping VisibleMetadataCallback) {
        let outcome: RequestOutcome<[VisibleMetadata]> = request.cancellation.isCancelled ? .cancelled : .succeeded([])
        done(RequestCompletion(id: request.id, outcome: outcome))
    }

    func watch(_ location: Location, changed: @escaping () -> Void) -> WatchHandle? { nil }
}

public protocol FileOperations: AnyObject {
    func inspectCapabilities(_ request: CapabilityRequest, done: @escaping CapabilityCallback)
    /// Operation-scoped inspection keeps a denied-access reason separate from an unsupported
    /// capability or an outdated version. The default is the plain inspection.
    func inspectOperationCapabilities(_ request: CapabilityRequest, capability: Capability, done: @escaping CapabilityCallback)
    /// Implementations recheck capability immediately before work, never overwrite by default,
    /// and report partial or unknown effects.
    func submit(_ request: OperationRequest, sink: @escaping OperationSink)
    /// An unsupported decision safely keeps the no-overwrite behaviour.
    func submit(_ request: OperationRequest, decision: ConflictDecision, sink: @escaping OperationSink)
}

public extension FileOperations {
    func inspectOperationCapabilities(_ request: CapabilityRequest, capability: Capability, done: @escaping CapabilityCallback) {
        inspectCapabilities(request, done: done)
    }

    func submit(_ request: OperationRequest, decision: ConflictDecision, sink: @escaping OperationSink) {
        submit(request, sink: sink)
    }
}

public protocol SearchRepository: AnyObject {
    func search(_ request: SearchRequest, done: @escaping SearchCallback)
}
