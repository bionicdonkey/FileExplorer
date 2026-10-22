import Foundation

public enum ScenarioKind: String { case adapter, domain }

/// One step of a scenario: exactly one of an action, an expectation, or a port script.
public struct Step {
    public enum Form { case action(String), expectation(String), portScript(String) }
    public let form: Form
    public let fields: JSON

    public var subject: String? { fields["subject"] as? String }
}

public struct Scenario {
    public let id: String
    public let kind: ScenarioKind
    public let summary: String
    public let given: JSON
    public let steps: [Step]
    public let platforms: JSON

    /// Applies `platforms.<platform>` overrides: `replace_step` and `skip_step`.
    public func effectiveSteps(platform: String) throws -> [Step] {
        var slots: [Step?] = steps.map { $0 }
        guard let override = platforms[platform] as? JSON else { return steps }
        if let replace = override["replace_step"] as? JSON {
            for (index, value) in replace {
                guard let i = Int(index), slots.indices.contains(i), let fields = value as? JSON else {
                    throw SpecError("\(id): bad replace_step \(index)")
                }
                slots[i] = try Scenario.parseStep(fields, where: "\(id) replace_step \(index)", vocabulary: nil)
            }
        }
        for skipped in (override["skip_step"] as? [Int]) ?? [] where slots.indices.contains(skipped) { slots[skipped] = nil }
        return slots.compactMap { $0 }
    }

    static func parseStep(_ fields: JSON, where context: String, vocabulary: Vocabulary?) throws -> Step {
        let hasDo = fields["do"] != nil, hasExpect = fields["expect"] != nil
        let isScript = fields["port"] != nil && !hasDo && !hasExpect
        guard (hasDo ? 1 : 0) + (hasExpect ? 1 : 0) + (isScript ? 1 : 0) == 1 else {
            throw SpecError("\(context): a step needs exactly one of `do`, `expect`, or a port script")
        }
        let form: Step.Form
        if let name = fields["do"] as? String {
            if let vocabulary, !vocabulary.actions.contains(name) { throw SpecError("\(context): unknown action \(name)") }
            form = .action(name)
        } else if let name = fields["expect"] as? String {
            if let vocabulary, !vocabulary.expectations.contains(name) { throw SpecError("\(context): unknown expectation \(name)") }
            form = .expectation(name)
        } else if let port = fields["port"] as? String {
            if let vocabulary, !vocabulary.ports.contains(port) { throw SpecError("\(context): unknown port \(port)") }
            guard fields["request"] != nil, fields["respond"] != nil || fields["emit"] != nil else {
                throw SpecError("\(context): a port script needs `request` and `respond` or `emit`")
            }
            form = .portScript(port)
        } else {
            throw SpecError("\(context): `do`, `expect` and `port` must be strings")
        }
        if let subject = fields["subject"] as? String, let vocabulary, !vocabulary.subjects.contains(subject) {
            throw SpecError("\(context): unknown subject \(subject)")
        }
        return Step(form: form, fields: fields)
    }
}

/// Every scenario file under `spec/scenarios`, strictly parsed.
public struct ScenarioLibrary {
    public let scenarios: [String: Scenario]

    static let topLevelKeys: Set<String> = ["id", "kind", "summary", "note", "given", "steps", "platforms"]

    public init(specRoot: URL, vocabulary: Vocabulary) throws {
        let base = specRoot.appendingPathComponent("scenarios").resolvingSymlinksInPath()
        var found: [String: Scenario] = [:]
        let files = FileManager.default.enumerator(at: base, includingPropertiesForKeys: nil)
        var problems: [String] = []
        while let url = files?.nextObject() as? URL {
            guard url.pathExtension == "json" else { continue }
            let relative = String(url.resolvingSymlinksInPath().path.dropFirst(base.path.count + 1).dropLast(".json".count))
            do {
                found[relative] = try ScenarioLibrary.parse(url, relative: relative, vocabulary: vocabulary)
            } catch {
                problems.append("\(error)")
            }
        }
        if !problems.isEmpty { throw SpecError(problems.sorted().joined(separator: "\n")) }
        scenarios = found
    }

    static func parse(_ url: URL, relative: String, vocabulary: Vocabulary) throws -> Scenario {
        let root = try Json.object(url)
        for key in root.keys where !topLevelKeys.contains(key) { throw SpecError("\(url.path): unknown key `\(key)`") }
        guard let id = root["id"] as? String, id == relative else {
            throw SpecError("\(url.path): id \(root["id"] ?? "nil") does not match its path (\(relative))")
        }
        guard let kindName = root["kind"] as? String, let kind = ScenarioKind(rawValue: kindName), relative.hasPrefix(kindName + "/") else {
            throw SpecError("\(url.path): kind \(root["kind"] ?? "nil") does not match its directory")
        }
        guard let summary = root["summary"] as? String, !summary.isEmpty else { throw SpecError("\(url.path): needs a summary") }
        guard let rawSteps = root["steps"] as? [JSON], !rawSteps.isEmpty else { throw SpecError("\(url.path): needs at least one step") }
        let steps = try rawSteps.enumerated().map { try Scenario.parseStep($0.element, where: "\(url.path) step \($0.offset)", vocabulary: vocabulary) }
        let platforms = (root["platforms"] as? JSON) ?? [:]
        for (platform, value) in platforms {
            guard ["linux", "macos"].contains(platform), let override = value as? JSON else {
                throw SpecError("\(url.path): bad platforms entry `\(platform)`")
            }
            if override["skip_step"] != nil, override["note"] == nil { throw SpecError("\(url.path): skip_step needs a note") }
            if let replace = override["replace_step"] as? JSON {
                for (index, value) in replace {
                    guard let fields = value as? JSON else { throw SpecError("\(url.path): replace_step \(index) must be a step") }
                    _ = try Scenario.parseStep(fields, where: "\(url.path) replace_step \(index)", vocabulary: vocabulary)
                }
            }
        }
        return Scenario(id: id, kind: kind, summary: summary, given: (root["given"] as? JSON) ?? [:], steps: steps, platforms: platforms)
    }
}
