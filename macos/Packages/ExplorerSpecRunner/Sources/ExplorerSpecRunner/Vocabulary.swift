import Foundation

/// The names a scenario may use, read from `spec/schema/scenario.schema.json` so the two cannot
/// drift. A step naming anything else is an error even when its feature is still `pending`.
public struct Vocabulary {
    public let actions: Set<String>
    public let expectations: Set<String>
    public let ports: Set<String>
    public let subjects: Set<String>

    public init(specRoot: URL) throws {
        let url = specRoot.appendingPathComponent("schema/scenario.schema.json")
        let schema = try Json.object(url)
        guard let defs = schema["$defs"] as? JSON, let step = defs["step"] as? JSON,
              let properties = step["properties"] as? JSON else {
            throw SpecError("\(url.path): no step definition")
        }
        func names(_ key: String) throws -> Set<String> {
            guard let property = properties[key] as? JSON, let list = property["enum"] as? [String] else {
                throw SpecError("\(url.path): step.\(key) has no enum")
            }
            return Set(list)
        }
        actions = try names("do")
        expectations = try names("expect")
        ports = try names("port")
        subjects = try names("subject")
    }
}
