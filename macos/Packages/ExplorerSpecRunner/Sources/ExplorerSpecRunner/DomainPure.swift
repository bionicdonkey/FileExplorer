import Foundation
import ExplorerDomain

/// Subjects that need no ports: `location`, `session`, `named_child`, `clipboard`, `pins`, and the
/// two checks that compare a contract table with the code.
final class PureSubject {
    let specRoot: URL
    var clipboard = ClipboardState()
    private var pinsDirectory: URL?
    private var pins: PinStore?

    init(specRoot: URL) { self.specRoot = specRoot }

    deinit { pinsDirectory.map { try? FileManager.default.removeItem(at: $0) } }

    /// A literal URI, `$name`, or `{"join": ["$name", "suffix"]}`.
    func text(_ world: DomainWorld, _ value: Any?) throws -> String {
        if let s = value as? String {
            guard s.hasPrefix("$") else { return s }
            let name = String(s.dropFirst())
            guard case .location(let l)? = world.bound[name] else { throw RunError("\(s) is not bound") }
            return l.uri
        }
        if let map = value as? JSON, let parts = map["join"] as? [Any] { return try parts.map { try text(world, $0) }.joined() }
        throw RunError("bad text \(value ?? "nil")")
    }

    // MARK: location

    func expectLocation(_ world: DomainWorld, _ step: JSON) throws {
        let input = try text(world, step["input"])
        let parsed: Result<Location, Error> = Result {
            step.string("via") == "input" ? try LocationInput.parse(input) : try Location(uri: input)
        }
        if step.string("is") == "rejected" {
            if case .success = parsed { throw RunError("\(input.debugDescription) was accepted, expected rejection") }
            return
        }
        let loc: Location
        switch parsed {
        case .success(let value): loc = value
        case .failure: throw RunError("\(input.debugDescription) was rejected")
        }
        if let want = step.string("canonical") { try expectEqual("canonical", loc.uri, want) }
        if let want = step.string("redacted") { try expectEqual("redacted", loc.redacted, want) }
        if let want = step.string("display_name") { try expectEqual("display_name", loc.displayName, want) }
        if step.has("parent") {
            let want: String? = step.isNull("parent") ? nil : try text(world, step["parent"])
            try expectEqual("parent", loc.parent?.uri, want)
        }
        if let want = step.bool("inside_archive") { try expectEqual("inside_archive", loc.isInsideArchive, want) }
        if step.isNull("local_path") { try expectEqual("local_path", loc.localPath, nil) }
        if step.has("equals") { try expectEqual("equality", loc == (try Location(uri: try text(world, step["equals"]))), true) }
        if step.has("differs_from") { try expectEqual("inequality", loc != (try Location(uri: try text(world, step["differs_from"]))), true) }
        var derived = loc
        if step.bool("archive_contents") == true || step.string("archive_contents") != nil {
            derived = try loc.archiveContents()
            if let want = step.string("archive_contents") { try expectEqual("archive_contents", derived.uri, want) }
        }
        if let name = step.string("as") { world.bound[name] = .location(derived) }
    }

    // MARK: session

    func expectSession(_ step: JSON) throws {
        if step.has("round_trip") {
            let locations = try step.strings("round_trip").map(location)
            try expectEqual("round trip", try SessionCodec.decode(try SessionCodec.encode(locations)), locations)
        }
        if let stored = step["decode"] {
            let data = try JSONSerialization.data(withJSONObject: stored)
            let decoded = try SessionCodec.decodeState(data)
            if step.has("locations") { try expectEqual("locations", decoded.locations.map(\.uri), step.strings("locations")) }
            if step.has("notices") { try expectEqual("notices", decoded.notices.map(\.rawValue), step.strings("notices")) }
            if let want = step.dict("rewritten_as") {
                let object = try JSONSerialization.jsonObject(with: try SessionCodec.encode(decoded.locations)) as? JSON ?? [:]
                try expectEqual("rewritten schema_version", object.int("schema_version"), want.int("schema_version"))
                try expectEqual("rewritten locations", object.strings("locations"), want.strings("locations"))
            }
        }
        if step.has("legacy_paths") {
            let paths = step.strings("legacy_paths")
            let first = SessionCodec.importLegacyLocalPaths(paths)
            try expectEqual("idempotent import", SessionCodec.importLegacyLocalPaths(paths), first)
            try expectEqual("imported", first.map(\.uri), step.strings("uris"))
        }
    }

    func expectNamedChild(_ step: JSON) throws {
        let parent = try location(try require(step.string("parent"), "needs `parent`"))
        let result = Result { try namedChild(of: parent, name: try require(step.string("name"), "needs `name`")) }
        switch (step.string("is"), result) {
        case ("rejected", .failure): break
        case ("accepted", .success(let child)): if let want = step.string("uri") { try expectEqual("child uri", child.uri, want) }
        case (let want, let got): throw RunError("named child result is \(got), expected \(want ?? "nil")")
        }
    }

    // MARK: clipboard

    private func action(_ name: String) throws -> ClipboardAction {
        switch name {
        case "copy": return .copy
        case "cut": return .cut
        default: throw RunError("unknown clipboard action \(name)")
        }
    }

    private func parsePayload(_ step: JSON) throws -> Result<ClipboardPayload, Error> {
        let body = try require(step.string("text"), "needs `text`")
        switch step.string("format") ?? "uri_list" {
        case "uri_list": return Result { try ClipboardPayload.parse(action: try action(step.string("action") ?? "copy"), text: body) }
        case "gnome": return Result { try ClipboardPayload.parseGnome(body) }
        case let other: throw RunError("unknown clipboard format \(other)")
        }
    }

    func expectClipboardPayload(_ step: JSON) throws {
        switch (step.string("is"), try parsePayload(step)) {
        case ("rejected", .failure): break
        case ("accepted", .success(let payload)):
            if let want = step.string("action") { try expectEqual("action", payload.action, try action(want)) }
            if step.has("uris") { try expectEqual("uris", payload.locations.map(\.uri), step.strings("uris")) }
            if let want = step.string("uri_list") { try expectEqual("uri_list", payload.uriList, want) }
        case (let want, let got): throw RunError("payload is \(got), expected \(want ?? "nil")")
        }
    }

    func clipboardAction(_ name: String, _ step: JSON) throws {
        switch name {
        case "replace":
            guard case .success(let payload) = try parsePayload(step) else { throw RunError("payload rejected") }
            clipboard.replace(payload)
        case "begin_paste": clipboard.beginPaste(try require(step.uint64("id"), "needs `id`"))
        case "finish_paste": clipboard.finishPaste(try require(step.uint64("id"), "needs `id`"), terminal: try require(step.bool("terminal"), "needs `terminal`"))
        default: throw RunError("clipboard has no action \(name)")
        }
    }

    func expectClipboard(_ step: JSON) throws {
        switch (step["mode"], clipboard.payload) {
        case (is NSNull, nil): break
        case (let mode as String, let payload?):
            try expectEqual("mode", payload.action, try action(mode))
            if step.has("uris") { try expectEqual("uris", payload.locations.map(\.uri), step.strings("uris")) }
        default: throw RunError("clipboard holds \(String(describing: clipboard.payload?.action)), expected mode \(step["mode"] ?? "nil")")
        }
    }

    // MARK: pins

    private func pinsFile() throws -> URL {
        if pinsDirectory == nil {
            let dir = FileManager.default.temporaryDirectory.appendingPathComponent("spec-pins-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            pinsDirectory = dir
        }
        return pinsDirectory!.appendingPathComponent("pins.json")
    }

    func pinsAction(_ name: String, _ step: JSON) throws {
        let file = try pinsFile()
        if pins == nil { pins = PinStore(file: file) }
        switch name {
        case "pin": pins!.pin(try location(try require(step.string("uri"), "needs `uri`")))
        case "save": try pins!.save()
        case "reload": pins = PinStore(file: file)
        default: throw RunError("pins has no action \(name)")
        }
    }

    func expectPins(_ step: JSON) throws {
        let got = try require(pins, "no pins loaded").pins.map(\.uri)
        for want in step.strings("contains") where !got.contains(want) { throw RunError("pins \(got) lack \(want)") }
    }

    // MARK: file types and errors

    func expectActivation(_ step: JSON) throws {
        let item = try Self.item(step, name: try require(step.string("name"), "needs `name`"), uri: step.string("uri"), contentType: step.string("content_type"))
        let got = activation(for: item)
        let name: String
        var browsesArchive = false
        switch got {
        case .browse(let l): name = "browse"; browsesArchive = l.isInsideArchive
        case .launchExternally: name = "launch_externally"
        }
        try expectEqual("activation", name, try require(step.string("is"), "needs `is`"))
        if let want = step.bool("browses_archive") {
            try expectEqual("browses an archive", browsesArchive, want)
            if want, case .browse(let l) = got { try expectEqual("archive display name", l.displayName, item.name) }
        }
        if let want = step.bool("extractable") { try expectEqual("extractable", isExtractable(item), want) }
    }

    static func item(_ step: JSON, name: String, uri: String?, contentType: String?) throws -> FileItem {
        let kind: EntryKind
        switch step.string("kind") ?? "file" {
        case "file": kind = .file
        case "directory": kind = .directory
        case let other: throw RunError("unknown entry kind \(other)")
        }
        let loc = try location(uri ?? "file:///home/user/\(name)")
        return FileItem(snapshot: EntrySnapshot(location: loc, displayName: name, kind: kind, contentType: contentType))
    }

    /// Every type and extension in the contract behaves as its rule says, locally and remotely.
    func expectFileTypesTable() throws {
        let table = try Json.object(specRoot.appendingPathComponent("contracts/file-types.json"))
        var checked = 0
        for rule in try require(table["rules"] as? [JSON], "rules must be an array") {
            let id = rule.string("id") ?? "?"
            let match = rule.dict("match") ?? [:]
            var subjects: [(name: String, type: String?)] = []
            for key in ["mime", "uti"] { subjects += match.strings(key).map { ("sample.bin", $0) } }
            subjects += match.strings("extensions").map { ("sample.\($0)", nil) }
            let wantLocal: Bool, wantRemote: Bool
            switch rule["browsable"] {
            case let value as Bool: (wantLocal, wantRemote) = (value, value)
            case let text as String where text == "local_only": (wantLocal, wantRemote) = (true, false)
            default: throw RunError("\(id): bad browsable \(rule["browsable"] ?? "nil")")
            }
            for (name, type) in subjects {
                let local = try Self.item([:], name: name, uri: nil, contentType: type)
                let remote = try Self.item([:], name: name, uri: "smb://server/\(name)", contentType: type)
                func browses(_ item: FileItem) -> Bool { if case .browse = activation(for: item) { return true } else { return false } }
                try expectEqual("\(id): \(name) \(type ?? "-") local browse", browses(local), wantLocal)
                try expectEqual("\(id): \(name) \(type ?? "-") remote browse", browses(remote), wantRemote)
                if let extractable = rule.bool("extractable") { try expectEqual("\(id): \(name) \(type ?? "-") extractable", isExtractable(local), extractable) }
                checked += 1
            }
        }
        if checked == 0 { throw RunError("the file-types table has no types or extensions to check") }
    }

    /// Every kind's message key and retry class, and every failure's kind, as the code has them.
    func expectErrorTable() throws {
        let table = try Json.object(specRoot.appendingPathComponent("contracts/errors.json"))
        let kinds = try require(table["kinds"] as? [JSON], "kinds must be an array")
        for entry in kinds {
            let name = entry.string("kind") ?? "?"
            let error = OperationError(try errorKind(name))
            try expectEqual("\(name) message key", error.messageKey, try require(entry.string("message_key"), "needs message_key"))
            let retry = try require(entry.string("retry"), "needs retry")
            let got: String
            switch error.retry {
            case .never: got = "never"
            case .afterRefresh: got = "after_refresh"
            case .afterUserAction: got = "after_user_action"
            case .transient: got = "transient"
            }
            try expectEqual("\(name) retry", got, retry)
        }
        for entry in try require(table["failures"] as? [JSON], "failures must be an array") {
            let name = entry.string("failure") ?? "?"
            let failure = try require(BackendFailure(rawValue: name), "unknown failure \(name)")
            try expectEqual("failure \(name)", failure.kind, try errorKind(try require(entry.string("kind"), "needs kind")))
        }
        // The table names exactly the kinds the code has, and exactly the failures it maps.
        try expectEqual("error kinds in the table", kinds.compactMap { $0.string("kind") }.sorted(), OperationErrorKind.allCases.map { errorKindName($0) }.sorted())
        let failures = (table["failures"] as? [JSON] ?? []).compactMap { $0.string("failure") }
        try expectEqual("failures in the table", failures.sorted(), BackendFailure.allCases.map(\.rawValue).sorted())
    }
}
