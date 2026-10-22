import Foundation

/// A scenario failure: the reason a step did not hold.
struct RunError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

extension Dictionary where Key == String, Value == Any {
    func string(_ key: String) -> String? { self[key] as? String }
    func bool(_ key: String) -> Bool? { (self[key] as? NSNumber).flatMap { CFGetTypeID($0) == CFBooleanGetTypeID() ? $0.boolValue : nil } }
    func int(_ key: String) -> Int? {
        guard let number = self[key] as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        return number.intValue
    }
    func uint64(_ key: String) -> UInt64? {
        guard let number = self[key] as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        return number.uint64Value
    }
    func dict(_ key: String) -> JSON? { self[key] as? JSON }
    func array(_ key: String) -> [Any]? { self[key] as? [Any] }
    func strings(_ key: String) -> [String] { (self[key] as? [Any])?.compactMap { $0 as? String } ?? [] }
    func has(_ key: String) -> Bool { self[key] != nil }
    func isNull(_ key: String) -> Bool { self[key] is NSNull }
}

func require<T>(_ value: T?, _ message: @autoclosure () -> String) throws -> T {
    guard let value else { throw RunError(message()) }
    return value
}

func expectEqual<T: Equatable>(_ field: String, _ got: T, _ want: T) throws {
    if got != want { throw RunError("\(field) is \(got), expected \(want)") }
}
