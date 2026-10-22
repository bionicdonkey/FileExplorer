import Foundation
import ExplorerAdapters
import ExplorerDomain

// The privileged helper: a launch daemon that performs one validated file operation per request,
// for the Morin Explorer app only. It re-validates every request, and it accepts connections only
// from a client whose code signature matches the app (and, in a signed build, its team).

final class Service: NSObject, ElevatedHelperProtocol {
    func perform(verb: String, source: String, destination: String?, reply: @escaping (String?) -> Void) {
        guard let verb = ElevatedRequest.Verb(rawValue: verb) else { return reply("operation.invalid-request") }
        reply(ElevatedPerformer.perform(ElevatedRequest(verb: verb, source: source, destination: destination)))
    }
}

final class Delegate: NSObject, NSXPCListenerDelegate {
    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection connection: NSXPCConnection) -> Bool {
        var requirement = "identifier \"\(ElevationConstants.appIdentifier)\" and anchor apple generic"
        if let team = Bundle.main.object(forInfoDictionaryKey: "MorinTeamIdentifier") as? String, !team.isEmpty {
            requirement += " and certificate leaf[subject.OU] = \"\(team)\""
        }
        connection.setCodeSigningRequirement(requirement)
        connection.exportedInterface = NSXPCInterface(with: ElevatedHelperProtocol.self)
        connection.exportedObject = Service()
        connection.resume()
        return true
    }
}

let delegate = Delegate()
let listener = NSXPCListener(machServiceName: ElevationConstants.helperLabel)
listener.delegate = delegate
listener.resume()
RunLoop.main.run()
