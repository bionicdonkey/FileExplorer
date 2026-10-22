import Foundation
import ServiceManagement
import ExplorerDomain

/// The XPC interface between the app and its privileged helper. One validated operation per call.
@objc public protocol ElevatedHelperProtocol {
    /// `error` is nil on success, else a short message key.
    func perform(verb: String, source: String, destination: String?, reply: @escaping (String?) -> Void)
}

public enum ElevationConstants {
    public static let helperLabel = "casa.alera.MorinExplorer.helper"
    public static let daemonPlist = "casa.alera.MorinExplorer.helper.plist"
    public static let appIdentifier = "casa.alera.MorinExplorer"
}

/// What runs inside the helper: re-validates the request (it never trusts the sender) and performs it.
public enum ElevatedPerformer {
    public static func perform(_ request: ElevatedRequest) -> String? {
        if request.violation != nil { return "operation.invalid-request" }
        let cancel = CancellationToken()
        var changed = false
        do {
            switch request.verb {
            case .create: try FileExecutor.perform(.create, request.source, nil, cancel, replace: false, changed: &changed) { _ in }
            case .copy: try FileExecutor.perform(.copy, request.source, request.destination, cancel, replace: false, changed: &changed) { _ in }
            case .move: try FileExecutor.perform(.move, request.source, request.destination, cancel, replace: false, changed: &changed) { _ in }
            case .trash: try FileExecutor.perform(.trash, request.source, nil, cancel, replace: false, changed: &changed) { _ in }
            case .delete: try FileExecutor.perform(.delete, request.source, nil, cancel, replace: false, changed: &changed) { _ in }
            }
            return nil
        } catch {
            return OperationError(failure: backendFailure(error)).messageKey
        }
    }
}

/// The app's side: registers the helper as a launch daemon and talks to it over XPC. Registration
/// needs a signed build and the user's approval in System Settings, so `status` tells the UI what
/// to ask for.
public final class ElevationClient {
    public enum Status: Equatable { case notRegistered, requiresApproval, enabled, unavailable }

    private var connection: NSXPCConnection?
    private let teamIdentifier: String?

    /// `teamIdentifier` pins the helper's and the app's code-signing requirement; nil (an unsigned
    /// development build) pins only the identifiers.
    public init(teamIdentifier: String? = Bundle.main.object(forInfoDictionaryKey: "MorinTeamIdentifier") as? String) {
        self.teamIdentifier = (teamIdentifier?.isEmpty ?? true) ? nil : teamIdentifier
    }

    private var service: SMAppService { SMAppService.daemon(plistName: ElevationConstants.daemonPlist) }

    public var status: Status {
        switch service.status {
        case .enabled: return .enabled
        case .requiresApproval: return .requiresApproval
        case .notRegistered: return .notRegistered
        case .notFound: return .unavailable
        @unknown default: return .unavailable
        }
    }

    /// Registers the daemon. Afterwards the user may have to approve it in System Settings.
    public func register() throws { try service.register() }
    public func openApprovalSettings() { SMAppService.openSystemSettingsLoginItems() }

    private func requirement(for identifier: String) -> String {
        var text = "identifier \"\(identifier)\" and anchor apple generic"
        if let teamIdentifier { text += " and certificate leaf[subject.OU] = \"\(teamIdentifier)\"" }
        return text
    }

    public func perform(_ request: ElevatedRequest, completion: @escaping (String?) -> Void) {
        if let violation = request.violation { _ = violation; return completion("operation.invalid-request") }
        let connection = self.connection ?? makeConnection()
        self.connection = connection
        let proxy = connection.remoteObjectProxyWithErrorHandler { _ in
            DispatchQueue.main.async { completion("operation.mount-unavailable") }
        } as? ElevatedHelperProtocol
        guard let proxy else { return completion("operation.failed") }
        proxy.perform(verb: request.verb.rawValue, source: request.source, destination: request.destination) { message in
            DispatchQueue.main.async { completion(message) }
        }
    }

    private func makeConnection() -> NSXPCConnection {
        let connection = NSXPCConnection(machServiceName: ElevationConstants.helperLabel, options: .privileged)
        connection.remoteObjectInterface = NSXPCInterface(with: ElevatedHelperProtocol.self)
        connection.setCodeSigningRequirement(requirement(for: ElevationConstants.helperLabel))
        connection.invalidationHandler = { [weak self] in self?.connection = nil }
        connection.resume()
        return connection
    }
}
