// Pure permission -> checklist-row content mapping for the helper's
// first screen: a list of every still-needed permission, each with an
// icon, a title, and a one-line explanation, plus an "Allow" action —
// modeled on OpenAI Codex's own "Enable Codex Computer Use" screen.
// No AppKit import — `symbolName` names an SF Symbol; the AppKit glue
// in `PolarizeSetupHelper` resolves it to a real `NSImage`.
import Foundation

public struct PermissionChecklistItem: Equatable {
    public let permission: NeededPermission
    public let title: String
    public let detail: String
    public let symbolName: String

    public init(permission: NeededPermission, title: String, detail: String, symbolName: String) {
        self.permission = permission
        self.title = title
        self.detail = detail
        self.symbolName = symbolName
    }
}

public enum PermissionChecklist {
    /// One row per `needed` permission, in the same order.
    public static func items(for needed: [NeededPermission]) -> [PermissionChecklistItem] {
        needed.map(item(for:))
    }

    public static func item(for permission: NeededPermission) -> PermissionChecklistItem {
        switch permission {
        case .accessibility:
            return PermissionChecklistItem(
                permission: permission,
                title: "Accessibility",
                detail: "Allows Polarize to see and control other apps' interfaces.",
                symbolName: "accessibility"
            )
        case .screenRecording:
            return PermissionChecklistItem(
                permission: permission,
                title: "Screen Recording",
                detail: "Polarize captures screenshots to see what is on screen.",
                symbolName: "camera.viewfinder"
            )
        case .automation(let target):
            return PermissionChecklistItem(
                permission: permission,
                title: "Automation (\(target))",
                detail: "Allows Polarize to send commands to \(target).",
                symbolName: "bolt.horizontal.circle"
            )
        case .unknown(let value):
            return PermissionChecklistItem(
                permission: permission,
                title: value,
                detail: "Open System Settings and enable Polarize.",
                symbolName: "questionmark.circle"
            )
        }
    }
}
