import Foundation

public enum RunMode { case normal, excluded }

/// Runs scenarios against this app. Returns nil on success or the reason for a failure.
public struct ScenarioRunner {
    public let spec: Spec
    public init(spec: Spec) { self.spec = spec }

    public func run(_ scenario: Scenario, mode: RunMode) -> String? {
        do {
            switch scenario.kind {
            case .domain: try DomainRunner.run(scenario, mode: mode)
            case .adapter: throw RunError("adapter scenarios are not implemented by this runner yet")
            }
            return nil
        } catch {
            return "\(error)"
        }
    }
}

public struct Selected {
    public let scenario: Scenario
    public let feature: String
    public let mode: RunMode
}

extension Spec {
    /// Scenarios of every feature whose macOS state is not `pending`.
    public func selected(platform: String = "macos", includePending: Bool = false) -> [Selected] {
        var out: [Selected] = []
        for feature in registry.features {
            let state = platform == "linux" ? feature.linux : feature.macos
            if state == .pending && !includePending { continue }
            for id in feature.scenarios {
                if let scenario = library.scenarios[id] {
                    out.append(Selected(scenario: scenario, feature: feature.id, mode: state == .excluded ? .excluded : .normal))
                }
            }
        }
        return out
    }
}

/// The report `spec-runner` prints, one line per scenario, in the format of the Linux runner.
public struct RunReport {
    public var lines: [String] = []
    public var failures = 0
    public init() {}
}

public func runSelected(spec: Spec, filters: [String], repeatCount: Int, includePending: Bool = false, out: (String) -> Void) -> Int {
    let runner = ScenarioRunner(spec: spec)
    let chosen = spec.selected(includePending: includePending).filter { selected in filters.isEmpty || filters.contains { selected.scenario.id.contains($0) } }
    out("running \(chosen.count) scenario(s) x \(repeatCount)")
    var failures = 0
    for selected in chosen {
        var failure: String?
        for round in 0..<repeatCount {
            if let message = runner.run(selected.scenario, mode: selected.mode) {
                failure = repeatCount > 1 ? "round \(round + 1): \(message)" : message
                break
            }
        }
        if let failure {
            failures += 1
            out("scenario \(selected.scenario.id) (\(selected.feature)) ... FAILED\n    \(failure)")
        } else {
            out("scenario \(selected.scenario.id) (\(selected.feature)) ... ok")
        }
    }
    out(failures > 0 ? "\nspec scenarios: \(failures) failed" : "\nspec scenarios: all passed")
    return failures
}
