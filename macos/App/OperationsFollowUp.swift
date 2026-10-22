import AppKit
import ExplorerAdapters
import ExplorerDomain

/// Reacts to operations that finished badly: asks what to do about a conflict, offers an
/// administrator retry after a permission failure, and says plainly when something failed.
final class OperationsFollowUp {
    private let services: AppServices
    private let window: () -> NSWindow?
    private var handled = Set<UInt64>()
    private var observer: NSObjectProtocol?

    init(services: AppServices, window: @escaping () -> NSWindow?) {
        self.services = services
        self.window = window
        // History loaded at launch was dealt with in an earlier session.
        handled = Set(services.operations.records.map(\.id))
        observer = NotificationCenter.default.addObserver(forName: .operationsChanged, object: nil, queue: .main) { [weak self] _ in self?.changed() }
    }

    deinit { observer.map(NotificationCenter.default.removeObserver) }

    private func changed() {
        for record in services.operations.records where !record.state.isActive {
            services.clipboard.finishPaste(record.id, terminal: true)
            guard handled.insert(record.id).inserted else { continue }
            guard case .failed(let key) = record.state else { continue }
            let kind = services.operations.summary(record.id)?.errorKind
            if kind == OperationErrorKind.conflict.rawValue { resolveConflict(record) }
            else if services.operations.isEligibleForElevatedRetry(record.id) { offerElevation(record) }
            else { report(record, key: key) }
        }
    }

    // MARK: conflicts

    private func resolveConflict(_ record: OperationRecord) {
        let preference = services.preferences.preferences.conflictDefault
        switch preference {
        case .replace: services.operations.resolveConflict(record.id, decision: .replace); return
        case .keepBoth: services.operations.resolveConflict(record.id, decision: .keepBoth); return
        case .skip: services.operations.resolveConflict(record.id, decision: .skip); return
        case .ask: break
        }
        let remaining = services.operations.summary(record.id)?.remaining ?? []
        let names = remaining.compactMap { try? Location(uri: $0).displayName }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = names.count == 1 ? "“\(names[0])” already exists" : "\(names.count) items already exist"
        alert.informativeText = "An item with the same name is already in the destination. What would you like to do?"
            + (names.count > 1 ? " Your choice applies to all \(names.count) items." : "")
        alert.addButton(withTitle: "Keep Both")
        alert.addButton(withTitle: "Replace")
        alert.addButton(withTitle: "Skip")
        alert.addButton(withTitle: "Stop")
        alert.buttons[1].hasDestructiveAction = true
        let decide: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            let decision: ConflictDecision
            switch response {
            case .alertFirstButtonReturn: decision = .keepBoth
            case .alertSecondButtonReturn: decision = .replace
            case .alertThirdButtonReturn: decision = .skip
            default: decision = .cancelRemaining
            }
            self?.services.operations.resolveConflict(record.id, decision: decision)
        }
        if let window = window() { alert.beginSheetModal(for: window, completionHandler: decide) } else { decide(alert.runModal()) }
    }

    // MARK: elevation

    private func offerElevation(_ record: OperationRecord) {
        guard let request = services.operations.elevatedRequest(for: record.id) else { return report(record, key: OperationError(.permissionDenied).messageKey) }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Administrator permission is needed"
        alert.informativeText = "You don’t have permission to \(Messages.title(for: record.kind.rawValue).lowercased()) “\(request.source.split(separator: "/").last.map(String.init) ?? request.source)”. "
            + "Morin Explorer can ask a privileged helper to do just this one thing."
        alert.addButton(withTitle: "Retry as Administrator")
        alert.addButton(withTitle: "Cancel")
        let run: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            if response == .alertFirstButtonReturn { self?.runElevated(request) }
        }
        if let window = window() { alert.beginSheetModal(for: window, completionHandler: run) } else { run(alert.runModal()) }
    }

    private func runElevated(_ request: ElevatedRequest) {
        let client = services.elevation
        switch client.status {
        case .unavailable:
            return inform("The helper isn’t available", "This copy of Morin Explorer doesn’t include its privileged helper, so the operation can’t be retried as administrator.")
        case .notRegistered:
            do { try client.register() } catch { return inform("Couldn’t enable the helper", error.localizedDescription) }
            if client.status == .requiresApproval { return askApproval(client) }
        case .requiresApproval:
            return askApproval(client)
        case .enabled:
            break
        }
        client.perform(request) { [weak self] message in
            if let message { self?.inform("Couldn’t complete the operation", Messages.text(forKey: message)) }
            else { NotificationCenter.default.post(name: .elevatedOperationFinished, object: nil) }
        }
    }

    private func askApproval(_ client: ElevationClient) {
        let alert = NSAlert()
        alert.messageText = "Approve the helper in System Settings"
        alert.informativeText = "Allow “Morin Explorer” under Login Items & Extensions, then try again."
        alert.addButton(withTitle: "Open System Settings")
        alert.addButton(withTitle: "Cancel")
        let run: (NSApplication.ModalResponse) -> Void = { response in if response == .alertFirstButtonReturn { client.openApprovalSettings() } }
        if let window = window() { alert.beginSheetModal(for: window, completionHandler: run) } else { run(alert.runModal()) }
    }

    // MARK: reporting

    private func report(_ record: OperationRecord, key: String) {
        inform("Couldn’t \(Messages.title(for: record.kind.rawValue).lowercased())", Messages.text(forKey: key))
    }

    private func inform(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        if let window = window() { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }
}

extension Notification.Name {
    static let elevatedOperationFinished = Notification.Name("casa.alera.MorinExplorer.elevatedOperationFinished")
}
