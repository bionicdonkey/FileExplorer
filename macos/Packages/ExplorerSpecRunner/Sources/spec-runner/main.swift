import ExplorerSpecRunner
import Foundation

// spec-runner [filter ...]    SPEC_ROOT=<dir>  SPEC_FILTER=<text>  SPEC_REPEAT=<n>
// Prints the status table, then runs every scenario of every feature that is not pending.
// Arguments that start with `-` are ignored.
let environment = ProcessInfo.processInfo.environment
var filters = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("-") }
if let filter = environment["SPEC_FILTER"] { filters.append(filter) }
let repeatCount = max(1, Int(environment["SPEC_REPEAT"] ?? "") ?? 1)

do {
    let spec = try Spec(root: try Spec.locate())
    print(spec.statusTable())
    let failures = runSelected(spec: spec, filters: Array(filters), repeatCount: repeatCount) { print($0) }
    exit(failures > 0 ? 1 : 0)
} catch {
    FileHandle.standardError.write(Data("spec error: \(error)\n".utf8))
    exit(2)
}
