import Foundation

// Saved state is bounded, atomic and defensive: one stale or hostile record is skipped with a
// notice rather than blocking startup, and nothing a user typed into a location is echoed back.

public enum StateLimits {
    public static let sessionTabs = 50
    public static let historyRecords = 100
    public static let stateBytes = 4 * 1024 * 1024
}

public enum RecoverableStateError: Error, CustomStringConvertible {
    case invalidState
    public var description: String { "saved location state could not be recovered" }
}

/// A user-safe explanation that a persisted record was skipped.
public enum RecoverableStateNotice: String, Equatable {
    case malformedRecord = "malformed_record"
    case unsupportedSchema = "unsupported_schema"
}

public struct DecodedURIState: Equatable {
    public var locations: [Location]
    public var notices: [RecoverableStateNotice]
}

public enum BoundedState {
    public static func read(_ url: URL) throws -> Data {
        let data = try Data(contentsOf: url)
        if data.count > StateLimits.stateBytes { throw RecoverableStateError.invalidState }
        return data
    }

    public static func write(_ data: Data, to url: URL) throws {
        if data.count > StateLimits.stateBytes { throw RecoverableStateError.invalidState }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: .atomic)
    }
}

/// Version 2 session state persists only canonical, credential-free URIs.
public enum SessionCodec {
    public static let version = 2

    public static func encode(_ locations: [Location]) throws -> Data {
        let object: [String: Any] = ["schema_version": version, "locations": locations.prefix(StateLimits.sessionTabs).map(\.uri)]
        let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
        if data.count > StateLimits.stateBytes { throw RecoverableStateError.invalidState }
        return data
    }

    public static func decode(_ data: Data) throws -> [Location] { try decodeState(data).locations }

    /// Decodes version 2 URI state, or the read-only version 1 path import. Invalid records are
    /// skipped, with a notice each, so one stale tab cannot block startup.
    public static func decodeState(_ data: Data) throws -> DecodedURIState {
        if data.count > StateLimits.stateBytes { throw RecoverableStateError.invalidState }
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let records = object["locations"] as? [Any] else { throw RecoverableStateError.invalidState }
        let version = (object["schema_version"] as? NSNumber)?.intValue
        var decoded: [Location] = []
        var notices: [RecoverableStateNotice] = []
        switch version {
        case SessionCodec.version:
            for value in records.prefix(StateLimits.sessionTabs) {
                if let text = value as? String, let location = try? Location(uri: text) { decoded.append(location) }
                else { notices.append(.malformedRecord) }
            }
        case nil, 1:
            for value in records.prefix(StateLimits.sessionTabs) {
                guard let text = value as? String else { notices.append(.malformedRecord); continue }
                if let location = try? Location(uri: text) {
                    if location.isLocal { decoded.append(location) } else { notices.append(.malformedRecord) }
                } else {
                    decoded.append(Location(localPath: text))
                }
            }
        default: throw RecoverableStateError.invalidState
        }
        return DecodedURIState(locations: decoded, notices: notices)
    }

    /// The read-only version 1 migration boundary. It writes nothing.
    public static func importLegacyLocalPaths(_ paths: [String]) -> [Location] { paths.map { Location(localPath: $0) } }
}

public enum ConflictDefault: String, Codable, Equatable {
    case ask, replace, keepBoth = "keep-both", skip
}

public struct ApplicationPreferences: Codable, Equatable {
    public var operationHistoryDays = 7
    public var closeOnLastTab = false
    public var restorePreviousTabs = false
    public var newTabLocation = Location.home.uri
    public var conflictDefault = ConflictDefault.ask
    public var contentSearchEnabled = false
    public var recordSearchHistory = true
    public var searchHistoryRetentionDays = 30

    enum CodingKeys: String, CodingKey {
        case operationHistoryDays = "operation_history_days", closeOnLastTab = "close_on_last_tab"
        case restorePreviousTabs = "restore_previous_tabs", newTabLocation = "new_tab_location"
        case conflictDefault = "conflict_default", contentSearchEnabled = "content_search_enabled"
        case recordSearchHistory = "record_search_history", searchHistoryRetentionDays = "search_history_retention_days"
    }

    public init() {}

    /// Missing fields fall back to their defaults, so an older file still loads.
    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = ApplicationPreferences()
        operationHistoryDays = try container.decodeIfPresent(Int.self, forKey: .operationHistoryDays) ?? defaults.operationHistoryDays
        closeOnLastTab = try container.decodeIfPresent(Bool.self, forKey: .closeOnLastTab) ?? defaults.closeOnLastTab
        restorePreviousTabs = try container.decodeIfPresent(Bool.self, forKey: .restorePreviousTabs) ?? defaults.restorePreviousTabs
        newTabLocation = try container.decodeIfPresent(String.self, forKey: .newTabLocation) ?? defaults.newTabLocation
        conflictDefault = try container.decodeIfPresent(ConflictDefault.self, forKey: .conflictDefault) ?? defaults.conflictDefault
        contentSearchEnabled = try container.decodeIfPresent(Bool.self, forKey: .contentSearchEnabled) ?? defaults.contentSearchEnabled
        recordSearchHistory = try container.decodeIfPresent(Bool.self, forKey: .recordSearchHistory) ?? defaults.recordSearchHistory
        searchHistoryRetentionDays = try container.decodeIfPresent(Int.self, forKey: .searchHistoryRetentionDays) ?? defaults.searchHistoryRetentionDays
    }
}

/// Preferences and the files beside them (session, operation history, search history), under one
/// folder: `~/Library/Application Support/MorinExplorer` by default.
public final class PreferencesStore {
    public let directory: URL
    public private(set) var preferences: ApplicationPreferences

    public static var defaultDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("MorinExplorer")
    }

    public init(directory: URL = PreferencesStore.defaultDirectory) {
        self.directory = directory
        let file = directory.appendingPathComponent("preferences.json")
        if let data = try? BoundedState.read(file), let loaded = try? JSONDecoder().decode(ApplicationPreferences.self, from: data) {
            preferences = loaded
        } else {
            preferences = ApplicationPreferences()
        }
    }

    public var historyFile: URL { directory.appendingPathComponent("operation-history.json") }
    public var sessionFile: URL { directory.appendingPathComponent("session.json") }
    public var searchHistoryFile: URL { directory.appendingPathComponent("search-history.json") }
    public var pinsFile: URL { directory.appendingPathComponent("pins.json") }
    public var viewOptionsFile: URL { directory.appendingPathComponent("view-options.json") }

    public func update(_ change: (inout ApplicationPreferences) -> Void) throws {
        change(&preferences)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try BoundedState.write(try encoder.encode(preferences), to: directory.appendingPathComponent("preferences.json"))
    }

    public func saveSession(_ locations: [Location]) throws {
        if !preferences.restorePreviousTabs { try clearSession(); return }
        try BoundedState.write(try SessionCodec.encode(locations), to: sessionFile)
    }

    public func loadSession() throws -> DecodedURIState {
        guard FileManager.default.fileExists(atPath: sessionFile.path) else { return DecodedURIState(locations: [], notices: []) }
        return try SessionCodec.decodeState(try BoundedState.read(sessionFile))
    }

    public func clearSession() throws {
        if FileManager.default.fileExists(atPath: sessionFile.path) { try FileManager.default.removeItem(at: sessionFile) }
    }
}
