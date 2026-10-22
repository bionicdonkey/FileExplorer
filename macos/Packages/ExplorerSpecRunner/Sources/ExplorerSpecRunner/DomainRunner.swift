import Foundation
import ExplorerDomain

/// `kind: "domain"` scenarios: the domain state machines over scripted ports.
enum DomainRunner {
    static func run(_ scenario: Scenario, mode: RunMode, specRoot: URL) throws {
        if mode == .excluded { throw RunError("excluded domain scenarios are not implemented by this runner yet") }
        let world = try DomainWorld(given: scenario.given, specRoot: specRoot)
        for (index, step) in try scenario.effectiveSteps(platform: "macos").enumerated() {
            do { try perform(step, in: world) } catch let error as RunError { throw RunError("step \(index): \(error)") }
        }
    }

    static func perform(_ step: Step, in world: DomainWorld) throws {
        let fields = step.fields
        switch step.form {
        case .action(let action):
            let subject = try require(fields.string("subject"), "`do` needs a `subject`")
            switch subject {
            case "navigation": try navigationAction(world, action, fields)
            case "operations": try world.operations.action(world, action, fields)
            case "reducer": try world.operations.reducerAction(action, fields)
            case "search": try world.search.action(world, action, fields)
            case "clock": try world.search.clockAction(action, fields)
            case "search_history": try world.search.historyAction(action, fields)
            case "clipboard": try world.pure.clipboardAction(action, fields)
            case "pins": try world.pure.pinsAction(action, fields)
            default: throw RunError("subject \(subject) is not implemented by the domain runner yet")
            }
        case .portScript(let port):
            switch port {
            case "directory": try scriptDirectory(world, fields)
            case "capabilities", "operations": try world.operations.scriptPort(world, fields)
            case "search": try world.search.scriptPort(fields)
            default: throw RunError("port \(port) is not implemented by the domain runner yet")
            }
        case .expectation(let name):
            switch name {
            case "port_requests" where fields.string("port") == "directory": try expectDirectoryRequests(world, fields)
            case "request_cancelled" where fields.string("port") == "directory": try expectDirectoryCancelled(world, fields)
            case "view" where fields.string("subject") == "navigation": try expectNavigationView(world, fields)
            case "port_requests" where ["capabilities", "operations"].contains(fields.string("port") ?? ""): try world.operations.expectPortRequests(fields)
            case "port_requests" where fields.string("port") == "search": try world.search.expectPortRequests(fields)
            case "request_cancelled" where fields.string("port") == "search": try world.search.expectCancelled(fields)
            case "view" where fields.string("subject") == "search": try world.search.expectView(world, fields)
            case "search_history": try world.search.expectHistory(fields)
            case "location": try world.pure.expectLocation(world, fields)
            case "session": try world.pure.expectSession(fields)
            case "named_child": try world.pure.expectNamedChild(fields)
            case "clipboard_payload": try world.pure.expectClipboardPayload(fields)
            case "clipboard": try world.pure.expectClipboard(fields)
            case "pins": try world.pure.expectPins(fields)
            case "activation": try world.pure.expectActivation(fields)
            case "file_types_table": try world.pure.expectFileTypesTable()
            case "error_table": try world.pure.expectErrorTable()
            case "record", "lock_keys", "submit_result", "cancel_result", "reducer": try world.operations.expectation(world, name, fields)
            default: throw RunError("expectation \(name) is not implemented by the domain runner yet")
            }
        }
    }

    // MARK: navigation

    static func navigationAction(_ world: DomainWorld, _ action: String, _ step: JSON) throws {
        switch action {
        case "navigate": world.navigation.navigate(to: try location(try require(step.string("to"), "navigate needs `to`")))
        case "back": world.navigation.back()
        case "forward": world.navigation.forward()
        case "refresh": world.navigation.refresh()
        case "retry": world.navigation.retry()
        case "close": world.navigation.closeTab()
        case "select": world.tab.selectionKeys = step.strings("items")
        case "unmount":
            let handled = world.tab.markUnavailable(root: try location(try require(step.string("location"), "unmount needs `location`")))
            if let want = step.bool("accepted"), want != handled { throw RunError("unmount handled is \(handled), expected \(want)") }
        case "show_section": world.tab.showSection(SidebarSection(title: step.string("title") ?? "Section"))
        default: throw RunError("navigation has no action \(action)")
        }
    }

    static func scriptDirectory(_ world: DomainWorld, _ step: JSON) throws {
        let entry = world.directory.held[try requestIndex(step, count: world.directory.held.count)]
        let request = entry.request
        let outcome: RequestOutcome<DirectorySnapshot>
        if step.string("respond") == "cancelled" {
            outcome = .cancelled
        } else if let respond = step.dict("respond"), let failed = respond.dict("failed") {
            outcome = .failed(OperationError(try errorKind(try require(failed.string("kind"), "failed needs `kind`")), source: request.location))
        } else if let respond = step.dict("respond"), let done = respond.dict("succeeded") {
            let requested = try done.string("snapshot_for").map(location) ?? request.location
            let resolved: Location
            if let uri = done.string("resolved") { resolved = try location(uri) }
            else if done.has("snapshot_for") { resolved = requested } else { resolved = request.location }
            let entries = try done.strings("entries").map { uri -> EntrySnapshot in
                let loc = try location(uri)
                return EntrySnapshot(location: loc, displayName: loc.displayName, kind: .file)
            }
            outcome = .succeeded(DirectorySnapshot(location: ResolvedLocation(requested: requested, resolved: resolved), entries: entries))
        } else {
            throw RunError("unknown `respond`: \(step["respond"] ?? "nil")")
        }
        let id = step.uint64("as_id").map { RequestID($0) } ?? request.id
        entry.sink(RequestCompletion(id: id, outcome: outcome))
    }

    static func expectDirectoryRequests(_ world: DomainWorld, _ step: JSON) throws {
        let held = world.directory.held
        if let count = step.int("count"), held.count != count {
            throw RunError("directory port received \(held.count) request(s), expected \(count)")
        }
        if let path = step.dict("last")?.string("path") {
            let last = try require(held.last, "no request received")
            try expectEqual("last request location", last.request.location.uri, path)
        }
    }

    static func expectDirectoryCancelled(_ world: DomainWorld, _ step: JSON) throws {
        let held = world.directory.held
        let index = try requestIndex(step, count: held.count)
        let want = step.bool("is") ?? true
        let got = held[index].request.cancellation.isCancelled
        if got != want { throw RunError("request \(index + 1) cancelled is \(got), expected \(want)") }
    }

    static func statusName(_ status: NavigationStatus) -> String {
        switch status {
        case .idle: return "idle"
        case .loading: return "loading"
        case .ready: return "ready"
        case .failed: return "failed"
        case .closed: return "closed"
        }
    }

    /// A deliberately small projection: location, back and forward targets, status, error kind.
    static func expectNavigationView(_ world: DomainWorld, _ step: JSON) throws {
        let tab = world.tab
        if let want = step.string("location") { try expectEqual("location", tab.location.uri, want) }
        if let want = step.string("status") { try expectEqual("status", statusName(tab.status), want) }
        for (field, got) in [("back", tab.history.backTarget?.uri), ("forward", tab.history.forwardTarget?.uri)] where step.has(field) {
            if step.isNull(field) { if got != nil { throw RunError("\(field) is \(got!), expected null") } }
            else { try expectEqual(field, got, step.string(field)) }
        }
        if let want = step.string("content") {
            let got: String
            switch tab.content {
            case .directory(let l): got = "directory:\(l.uri)"
            case .sectionOverview: got = "section"
            case .search: got = "search"
            case .unavailable(let l): got = "unavailable:\(l.uri)"
            }
            try expectEqual("content", got, want)
        }
        for (field, got) in [("has_directory", tab.directory != nil), ("available", tab.hasAvailableDirectory),
                             ("pending", tab.hasPendingNavigation), ("can_go_back", tab.history.canGoBack),
                             ("can_go_forward", tab.history.canGoForward)] {
            if let want = step.bool(field), want != got { throw RunError("\(field) is \(got), expected \(want)") }
        }
        if step.has("current") {
            let got = tab.history.current?.uri
            if step.isNull("current") { if got != nil { throw RunError("current is \(got!), expected null") } }
            else { try expectEqual("current", got, step.string("current")) }
        }
        if step.has("selection") { try expectEqual("selection", tab.selectionKeys, step.strings("selection")) }
        if let want = step.string("failed_location") {
            guard case .failed(let location, _) = tab.status else { throw RunError("status is not failed") }
            try expectEqual("failed_location", location.uri, want)
        }
        if let want = step.string("error") {
            guard case .failed(_, let error) = tab.status else { throw RunError("status is not failed") }
            try expectEqual("error", error.kind, try errorKind(want))
        }
    }
}
