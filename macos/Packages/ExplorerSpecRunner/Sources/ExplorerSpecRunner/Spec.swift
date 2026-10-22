import Foundation

/// The whole specification: registry, vocabulary and every scenario, parsed and cross-checked.
public struct Spec {
    public let root: URL
    public let registry: Registry
    public let vocabulary: Vocabulary
    public let library: ScenarioLibrary

    /// Finds `spec/` from `SPEC_ROOT`, then upward from `start` (a working directory or source file).
    public static func locate(from start: URL? = nil) throws -> URL {
        if let env = ProcessInfo.processInfo.environment["SPEC_ROOT"] {
            return URL(fileURLWithPath: env, isDirectory: true)
        }
        var directory = (start ?? URL(fileURLWithPath: FileManager.default.currentDirectoryPath)).standardizedFileURL
        for _ in 0..<12 {
            let candidate = directory.appendingPathComponent("spec/features.json")
            if FileManager.default.fileExists(atPath: candidate.path) { return directory.appendingPathComponent("spec", isDirectory: true) }
            directory = directory.deletingLastPathComponent()
        }
        throw SpecError("spec/ not found; set SPEC_ROOT")
    }

    public init(root: URL) throws {
        self.root = root
        registry = try Registry(specRoot: root)
        vocabulary = try Vocabulary(specRoot: root)
        library = try ScenarioLibrary(specRoot: root, vocabulary: vocabulary)
        var problems: [String] = []
        var referenced = Set<String>()
        for feature in registry.features {
            for id in feature.scenarios {
                referenced.insert(id)
                if library.scenarios[id] == nil { problems.append("\(feature.id): references missing scenario \(id)") }
            }
            if feature.macos == .approximate {
                for id in feature.scenarios where library.scenarios[id]?.platforms["macos"] == nil {
                    problems.append("\(feature.id): macos is approximate but \(id) has no platforms.macos override")
                }
            }
        }
        for id in library.scenarios.keys where !referenced.contains(id) { problems.append("scenario \(id) is referenced by no feature") }
        if !problems.isEmpty { throw SpecError(problems.sorted().joined(separator: "\n")) }
    }

    /// The per-platform state table, as `spec-tools status` prints it.
    public func statusTable() -> String {
        let states: [FeatureState] = [.parity, .approximate, .excluded, .pending]
        var out = "platform".padding(toLength: 12, withPad: " ", startingAt: 0)
        for state in states { out += String(repeating: " ", count: 13 - state.rawValue.count) + state.rawValue }
        out += "   total\n"
        for platform in ["linux", "macos"] {
            out += platform.padding(toLength: 12, withPad: " ", startingAt: 0)
            for state in states {
                let count = registry.features.filter { (platform == "linux" ? $0.linux : $0.macos) == state }.count
                out += String(repeating: " ", count: 13 - String(count).count) + String(count)
            }
            out += String(repeating: " ", count: 8 - String(registry.features.count).count) + "\(registry.features.count)\n"
        }
        let pending = registry.features.filter { $0.macos == .pending }.map(\.id)
        if !pending.isEmpty { out += "\npending on macos (\(pending.count)):\n" + pending.map { "  \($0)" }.joined(separator: "\n") + "\n" }
        return out
    }
}
