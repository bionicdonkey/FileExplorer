import Foundation

public enum FeatureState: String {
    case parity, approximate, excluded, pending
}

public struct Feature {
    public let id: String
    public let title: String
    public let readme: String
    public let scenarios: [String]
    public let linux: FeatureState
    public let macos: FeatureState
    public let macosNote: String?
    public let uiTests: [String: [String]]
}

/// `spec/features.json`, strictly decoded: an unknown state or a missing field is an error.
public struct Registry {
    public let features: [Feature]

    public init(specRoot: URL) throws {
        let url = specRoot.appendingPathComponent("features.json")
        let root = try Json.object(url)
        guard root["version"] as? Int == 1 else { throw SpecError("\(url.path): version must be 1") }
        guard let list = root["features"] as? [JSON] else { throw SpecError("\(url.path): features must be an array") }
        var seen = Set<String>()
        features = try list.map { entry in
            func string(_ key: String) throws -> String {
                guard let value = entry[key] as? String, !value.isEmpty else {
                    throw SpecError("\(url.path): feature \(entry["id"] ?? "?") needs a string `\(key)`")
                }
                return value
            }
            let id = try string("id")
            guard seen.insert(id).inserted else { throw SpecError("\(url.path): duplicate feature id \(id)") }
            func state(_ platform: String) throws -> (FeatureState, String?) {
                guard let object = entry[platform] as? JSON, let name = object["state"] as? String,
                      let parsed = FeatureState(rawValue: name) else {
                    throw SpecError("\(url.path): \(id) has no valid `\(platform).state`")
                }
                let note = object["note"] as? String
                if (parsed == .approximate || parsed == .excluded), note == nil {
                    throw SpecError("\(url.path): \(id) \(platform) is \(parsed.rawValue) but has no note")
                }
                return (parsed, note)
            }
            let (linux, _) = try state("linux")
            let (macos, macosNote) = try state("macos")
            guard let scenarios = entry["scenarios"] as? [String] else {
                throw SpecError("\(url.path): \(id) needs a `scenarios` array")
            }
            var uiTests: [String: [String]] = [:]
            if let ui = entry["ui_tests"] as? [String: [String]] { uiTests = ui }
            return Feature(id: id, title: try string("title"), readme: try string("readme"), scenarios: scenarios,
                           linux: linux, macos: macos, macosNote: macosNote, uiTests: uiTests)
        }
    }
}
