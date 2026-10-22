import Foundation

// Owned values exchanged across the ports. Snapshots hold no live handles, no raw backend
// messages and no paths that were not asked for. Request ids are allocated by the application and
// are unique for the life of the process; operation ids are a separate namespace.

public struct RequestID: Hashable, Comparable {
    public let value: UInt64
    public init(_ value: UInt64) { self.value = value }
    public static func < (lhs: RequestID, rhs: RequestID) -> Bool { lhs.value < rhs.value }

    private static let lock = NSLock()
    private static var next: UInt64 = 1
    /// The shared namespace for directory, capability and search requests.
    public static func allocate() -> RequestID {
        lock.lock(); defer { lock.unlock() }
        defer { next += 1 }
        return RequestID(next)
    }
}

public struct OperationID: Hashable, Comparable {
    public let value: UInt64
    public init(_ value: UInt64) { self.value = value }
    public static func < (lhs: OperationID, rhs: OperationID) -> Bool { lhs.value < rhs.value }
}

/// Cancellation is intent: it asks an adapter to stop. The adapter still delivers a terminal answer.
public final class CancellationToken {
    public private(set) var isCancelled = false
    private var handlers: [() -> Void] = []
    private let lock = NSLock()

    public init() {}

    public func cancel() {
        lock.lock()
        if isCancelled { lock.unlock(); return }
        isCancelled = true
        let pending = handlers
        handlers = []
        lock.unlock()
        pending.forEach { $0() }
    }

    /// Runs `handler` when cancelled, or immediately if it already was.
    public func onCancel(_ handler: @escaping () -> Void) {
        lock.lock()
        if isCancelled { lock.unlock(); handler(); return }
        handlers.append(handler)
        lock.unlock()
    }
}

public enum Capability: String, CaseIterable, Hashable {
    case list, copy, move, create, rename, trash, delete, open, searchFilename, searchContent
}

/// Relations must come from a trusted provider, never from guessing at paths.
public enum Containment: Equatable {
    case opaque
    case knownAncestors([Location])
}

/// Absence from `supported` means unavailable, including an unknown capability.
public struct CapabilitySnapshot: Equatable {
    public var version: UInt64
    public var supported: Set<Capability>
    public var stableOrder: Bool
    public var containment: Containment

    public init(version: UInt64 = 0, supported: Set<Capability> = [], stableOrder: Bool = false, containment: Containment = .opaque) {
        self.version = version
        self.supported = supported
        self.stableOrder = stableOrder
        self.containment = containment
    }

    public func supports(_ capability: Capability) -> Bool { supported.contains(capability) }
}

public enum MutationKind: String, Equatable {
    case copy, move, create, rename, trash, delete

    public var capability: Capability {
        switch self {
        case .copy: return .copy
        case .move: return .move
        case .create: return .create
        case .rename: return .rename
        case .trash: return .trash
        case .delete: return .delete
        }
    }
}

public enum ConflictDecision: String, Equatable {
    case replace, keepBoth, skip, cancelRemaining
}

public struct OperationRequest {
    public var id: OperationID
    public var kind: MutationKind
    public var sources: [Location]
    public var destination: Location?
    public var explicitTarget: Location?
    public var capabilityVersion: UInt64
    public var cancellation: CancellationToken

    public init(id: OperationID, kind: MutationKind, sources: [Location], destination: Location? = nil,
                explicitTarget: Location? = nil, capabilityVersion: UInt64 = 1, cancellation: CancellationToken = CancellationToken()) {
        self.id = id
        self.kind = kind
        self.sources = sources
        self.destination = destination
        self.explicitTarget = explicitTarget
        self.capabilityVersion = capabilityVersion
        self.cancellation = cancellation
    }
}

public enum OperationErrorKind: String, CaseIterable, Equatable {
    case permissionDenied, authenticationRequired, mountUnavailable, conflict, notFound, readOnly
    case unsupported, capabilityChanged, partialCompletion, invalidRequest, unknown
}

public enum RetryClass: String, Equatable {
    case never, afterRefresh, afterUserAction, transient
}

/// The kind fixes the message key and the retry class (`spec/contracts/errors.json`). No backend
/// text, code or path crosses a port.
public struct OperationError: Error, Equatable, CustomStringConvertible {
    public var kind: OperationErrorKind
    public var messageKey: String
    public var source: String?
    public var destination: String?
    public var retry: RetryClass

    public init(_ kind: OperationErrorKind, source: Location? = nil, destination: Location? = nil) {
        self.kind = kind
        (messageKey, retry) = OperationError.table(kind)
        self.source = source?.redacted
        self.destination = destination?.redacted
    }

    public var description: String { messageKey }

    static func table(_ kind: OperationErrorKind) -> (String, RetryClass) {
        switch kind {
        case .permissionDenied: return ("operation.permission-denied", .afterUserAction)
        case .authenticationRequired: return ("operation.authentication-required", .afterUserAction)
        case .mountUnavailable: return ("operation.mount-unavailable", .afterUserAction)
        case .conflict: return ("operation.conflict", .afterUserAction)
        case .notFound: return ("operation.not-found", .afterRefresh)
        case .readOnly: return ("operation.read-only", .afterUserAction)
        case .unsupported: return ("operation.unsupported", .never)
        case .capabilityChanged: return ("operation.capability-changed", .afterRefresh)
        case .partialCompletion: return ("operation.partial-completion", .afterRefresh)
        case .invalidRequest: return ("operation.invalid-request", .never)
        case .unknown: return ("operation.failed", .afterRefresh)
        }
    }
}

public enum MutationEffect: String, Equatable {
    case unchanged, changed, partiallyChanged = "partially_changed", unknown
}

/// No rollback or atomicity is implied. `remaining` includes failed and unattempted sources; an
/// adapter with uncertain side effects must report `unknown`.
public struct OperationCompletion: Equatable {
    public var effect: MutationEffect
    public var completed: [Location]
    public var remaining: [Location]

    public init(effect: MutationEffect = .unchanged, completed: [Location] = [], remaining: [Location] = []) {
        self.effect = effect
        self.completed = completed
        self.remaining = remaining
    }
}

public struct CancellationOutcome: Equatable {
    public var completion: OperationCompletion
    public init(completion: OperationCompletion) { self.completion = completion }
}

public struct OperationProgress: Equatable {
    public var completedItems: UInt64
    public var totalItems: UInt64?
    public var completedBytes: UInt64?
    public var totalBytes: UInt64?

    public init(completedItems: UInt64 = 0, totalItems: UInt64? = nil, completedBytes: UInt64? = nil, totalBytes: UInt64? = nil) {
        self.completedItems = completedItems
        self.totalItems = totalItems
        self.completedBytes = completedBytes
        self.totalBytes = totalBytes
    }
}

public enum OperationEvent: Equatable {
    case started(id: OperationID, sequence: UInt64)
    case progress(id: OperationID, sequence: UInt64, progress: OperationProgress)
    case cancelRequested(id: OperationID, sequence: UInt64)
    case succeeded(id: OperationID, sequence: UInt64, completion: OperationCompletion)
    case failed(id: OperationID, sequence: UInt64, error: OperationError, completion: OperationCompletion)
    case cancelled(id: OperationID, sequence: UInt64, outcome: CancellationOutcome)

    public var id: OperationID {
        switch self {
        case .started(let id, _), .progress(let id, _, _), .cancelRequested(let id, _),
             .succeeded(let id, _, _), .failed(let id, _, _, _), .cancelled(let id, _, _): return id
        }
    }

    public var sequence: UInt64 {
        switch self {
        case .started(_, let s), .progress(_, let s, _), .cancelRequested(_, let s),
             .succeeded(_, let s, _), .failed(_, let s, _, _), .cancelled(_, let s, _): return s
        }
    }

    public var isTerminal: Bool {
        switch self {
        case .succeeded, .failed, .cancelled: return true
        default: return false
        }
    }

    public var name: String {
        switch self {
        case .started: return "started"
        case .progress: return "progress"
        case .cancelRequested: return "cancel_requested"
        case .succeeded: return "succeeded"
        case .failed: return "failed"
        case .cancelled: return "cancelled"
        }
    }

    public var completion: OperationCompletion? {
        switch self {
        case .succeeded(_, _, let c), .failed(_, _, _, let c): return c
        case .cancelled(_, _, let o): return o.completion
        default: return nil
        }
    }
}

/// Append-only acceptance gate. The event owner allocates sequences (including `cancelRequested`);
/// adapters must not allocate a competing namespace. Cancellation intent alone does not release
/// resources or imply completion.
public struct OperationReducer {
    public let id: OperationID
    public private(set) var events: [OperationEvent] = []

    public init(id: OperationID) { self.id = id }

    @discardableResult
    public mutating func apply(_ event: OperationEvent) -> Bool {
        if event.id != id || isTerminal { return false }
        if let last = events.last, event.sequence <= last.sequence { return false }
        events.append(event)
        return true
    }

    public var isTerminal: Bool { events.last?.isTerminal ?? false }
}

public enum EntryKind: String, Equatable {
    case file, directory, shortcut, other
}

public struct EntrySnapshot: Equatable {
    public var location: Location
    public var displayName: String
    public var kind: EntryKind
    public var size: UInt64?
    public var modified: Date?
    public var contentType: String?
    public var capabilities: CapabilitySnapshot

    public init(location: Location, displayName: String, kind: EntryKind, size: UInt64? = nil, modified: Date? = nil,
                contentType: String? = nil, capabilities: CapabilitySnapshot = CapabilitySnapshot()) {
        self.location = location
        self.displayName = displayName
        self.kind = kind
        self.size = size
        self.modified = modified
        self.contentType = contentType
        self.capabilities = capabilities
    }
}

public struct PageRequest: Equatable {
    public var offset: Int
    public var limit: Int
    public static let all = PageRequest(offset: 0, limit: Int.max)
    public init(offset: Int, limit: Int) {
        self.offset = offset
        self.limit = limit
    }
}

public struct DirectoryRequest {
    public var id: RequestID
    public var location: Location
    public var page: PageRequest
    public var cancellation: CancellationToken

    public init(id: RequestID = .allocate(), location: Location, page: PageRequest = .all, cancellation: CancellationToken = CancellationToken()) {
        self.id = id
        self.location = location
        self.page = page
        self.cancellation = cancellation
    }
}

public struct DirectorySnapshot: Equatable {
    public var location: ResolvedLocation
    public var entries: [EntrySnapshot]
    public var capabilities: CapabilitySnapshot
    public var nextPage: PageRequest?

    public init(location: ResolvedLocation, entries: [EntrySnapshot] = [], capabilities: CapabilitySnapshot = CapabilitySnapshot(), nextPage: PageRequest? = nil) {
        self.location = location
        self.entries = entries
        self.capabilities = capabilities
        self.nextPage = nextPage
    }
}

/// Advisory attributes loaded after a row becomes visible. Identity and core actions never wait on them.
public struct VisibleMetadata: Equatable {
    public var location: Location
    public var size: UInt64?
    public var modified: Date?
    public var contentType: String?
    public init(location: Location, size: UInt64? = nil, modified: Date? = nil, contentType: String? = nil) {
        self.location = location
        self.size = size
        self.modified = modified
        self.contentType = contentType
    }
}

public struct VisibleMetadataRequest {
    public var id: RequestID
    public var locations: [Location]
    public var cancellation: CancellationToken
    public init(id: RequestID = .allocate(), locations: [Location], cancellation: CancellationToken = CancellationToken()) {
        self.id = id
        self.locations = locations
        self.cancellation = cancellation
    }
}

public struct CapabilityRequest {
    public var id: RequestID
    public var location: Location
    public var cancellation: CancellationToken
    public init(id: RequestID = .allocate(), location: Location, cancellation: CancellationToken = CancellationToken()) {
        self.id = id
        self.location = location
        self.cancellation = cancellation
    }
}

public enum RequestOutcome<Value> {
    case succeeded(Value)
    case failed(OperationError)
    case cancelled
}

public struct RequestCompletion<Value> {
    public var id: RequestID
    public var outcome: RequestOutcome<Value>
    public init(id: RequestID, outcome: RequestOutcome<Value>) {
        self.id = id
        self.outcome = outcome
    }
}

public struct SearchScope: Equatable {
    public var root: Location
    /// The root is depth 0; a value of 1 includes direct children.
    public var maxDepth: UInt32
    public var maxResults: Int
    public var timeout: TimeInterval

    public init(root: Location, maxDepth: UInt32, maxResults: Int, timeout: TimeInterval) {
        self.root = root
        self.maxDepth = maxDepth
        self.maxResults = maxResults
        self.timeout = timeout
    }

    /// The default for a local search: direct children, 5000 results, two seconds.
    public static func localFallback(root: Location) -> SearchScope {
        SearchScope(root: root, maxDepth: 1, maxResults: 5000, timeout: 2)
    }
}

public enum SearchMode: String, Equatable { case filename, content }
public enum SearchLimit: String, Equatable { case depth, resultCount = "result_count", time }
public enum SearchIncompleteReason: String, Equatable {
    case cancelled, providerFailure = "provider_failure", malformedPage = "malformed_page", accessLost = "access_lost"
}

public struct SearchRequest {
    public var id: RequestID
    public var scope: SearchScope
    public var query: String
    public var mode: SearchMode
    public var cancellation: CancellationToken
    public init(id: RequestID = .allocate(), scope: SearchScope, query: String, mode: SearchMode, cancellation: CancellationToken = CancellationToken()) {
        self.id = id
        self.scope = scope
        self.query = query
        self.mode = mode
        self.cancellation = cancellation
    }
}

public enum SearchOutcome: Equatable {
    case completed(matches: [EntrySnapshot], declaredScope: SearchScope, scopeLimited: Bool, limits: [SearchLimit])
    case incomplete(matches: [EntrySnapshot], reason: SearchIncompleteReason, declaredScope: SearchScope, retry: RetryClass, error: OperationError?)
    case unsupported(declaredScope: SearchScope, error: OperationError)
}

public struct SearchCompletion {
    public var id: RequestID
    public var outcome: SearchOutcome
    public init(id: RequestID, outcome: SearchOutcome) {
        self.id = id
        self.outcome = outcome
    }
}

/// The backend failures an adapter can meet, named independently of any platform
/// (`spec/contracts/errors.json`, "failures"). An adapter maps its own errors (errno values,
/// `CocoaError` codes) onto these, and the table below maps them onto an error kind.
public enum BackendFailure: String, CaseIterable {
    case permissionDenied = "permission_denied", notMounted = "not_mounted", hostNotFound = "host_not_found"
    case notConnected = "not_connected", exists, wouldMerge = "would_merge", notFound = "not_found"
    case readOnly = "read_only", notSupported = "not_supported", invalidArgument = "invalid_argument"
    case invalidFilename = "invalid_filename", other

    public var kind: OperationErrorKind {
        switch self {
        case .permissionDenied: return .permissionDenied
        case .notMounted, .hostNotFound, .notConnected: return .mountUnavailable
        case .exists, .wouldMerge: return .conflict
        case .notFound: return .notFound
        case .readOnly: return .readOnly
        case .notSupported: return .unsupported
        case .invalidArgument, .invalidFilename: return .invalidRequest
        case .other: return .unknown
        }
    }
}

public extension OperationError {
    init(failure: BackendFailure, source: Location? = nil, destination: Location? = nil) {
        self.init(failure.kind, source: source, destination: destination)
    }
}
