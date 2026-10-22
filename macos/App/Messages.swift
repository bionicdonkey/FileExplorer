import Foundation

/// User-facing text for the message keys the domain reports; backend text never reaches the user.
enum Messages {
    static func text(forKey key: String) -> String {
        switch key {
        case "operation.permission-denied": return "You don’t have permission to do that."
        case "operation.authentication-required": return "This location needs credentials."
        case "operation.mount-unavailable": return "The volume is not available."
        case "operation.conflict": return "An item with that name already exists."
        case "operation.not-found": return "The item no longer exists. Refresh and try again."
        case "operation.read-only": return "This location is read-only."
        case "operation.unsupported": return "This isn’t supported here."
        case "operation.capability-changed": return "This location changed; refresh and try again."
        case "operation.partial-completion": return "Some items were not processed."
        case "operation.invalid-request": return "That can’t be done."
        default: return "The operation did not complete."
        }
    }

    static func title(for kind: String) -> String {
        switch kind {
        case "copy": return "Copy"
        case "move": return "Move"
        case "create": return "New Folder"
        case "rename": return "Rename"
        case "trash": return "Move to Trash"
        case "delete": return "Delete"
        case "extract": return "Extract"
        case "compress": return "Compress"
        default: return kind.capitalized
        }
    }
}
