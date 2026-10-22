import Foundation

/// A problem with the specification files themselves, as opposed to a failing scenario.
public struct SpecError: Error, CustomStringConvertible {
    public let description: String
    public init(_ description: String) { self.description = description }
}

public typealias JSON = [String: Any]

enum Json {
    static func read(_ url: URL) throws -> Any {
        let data: Data
        do { data = try Data(contentsOf: url) } catch { throw SpecError("\(url.path): \(error.localizedDescription)") }
        do { return try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) } catch {
            throw SpecError("\(url.path): \(error.localizedDescription)")
        }
    }

    static func object(_ url: URL) throws -> JSON {
        guard let value = try read(url) as? JSON else { throw SpecError("\(url.path): expected a JSON object") }
        return value
    }
}
