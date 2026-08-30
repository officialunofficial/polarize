// Pure permission -> checklist-row content mapping for the helper's
// first screen: a list of every still-needed permission, each with an
// icon, a title, and a one-line explanation, plus an "Allow" action —
// modeled on OpenAI Codex's own "Enable Codex Computer Use" screen.
// No AppKit import — `graphicIconUTI` and `symbolName` both name an
// icon by identifier only; the AppKit glue in `PolarizeSetupHelper`
// resolves either to a real `NSImage` (see `PermissionIcon.swift`).
import Foundation

public struct PermissionChecklistItem: Equatable {
    public let permission: NeededPermission
    public let title: String
    public let detail: String
    /// The UTI System Settings' own Privacy & Security extension uses
    /// for this permission's pane icon — e.g.
    /// `com.apple.graphic-icon.accessibility`. `nil` when no matching
    /// system pane icon exists (`.unknown`). Resolving this via
    /// `UTType` + `NSWorkspace.shared.icon(for:)` gives the exact icon
    /// System Settings itself shows, confirmed live against a real
    /// macOS 27 session. Prefer this over `symbolName` whenever it
    /// resolves; `symbolName` is the fallback for an OS where the UTI
    /// isn't declared.
    public let graphicIconUTI: String?
    public let symbolName: String

    public init(
        permission: NeededPermission,
        title: String,
        detail: String,
        graphicIconUTI: String?,
        symbolName: String
    ) {
        self.permission = permission
        self.title = title
        self.detail = detail
        self.graphicIconUTI = graphicIconUTI
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
                detail:
                    "Allows Polarize to see and control other apps' interfaces. "
                    + "Some macOS versions call this pane \"Device Control and Data Access\" instead.",
                graphicIconUTI: "com.apple.graphic-icon.accessibility",
                symbolName: "accessibility"
            )
        case .screenRecording:
            return PermissionChecklistItem(
                permission: permission,
                title: "Screen Recording",
                detail:
                    "Polarize captures screenshots to see what is on screen. "
                    + "Some macOS versions call this pane \"Screen & System Audio Recording\" instead.",
                graphicIconUTI: "com.apple.graphic-icon.screen-recording",
                symbolName: "record.circle.fill"
            )
        case .automation(let target):
            return PermissionChecklistItem(
                permission: permission,
                title: "Automation (\(target))",
                detail: "Allows Polarize to send commands to \(target).",
                graphicIconUTI: "com.apple.graphic-icon.automation",
                symbolName: "gearshape.2.fill"
            )
        case .unknown(let value):
            return PermissionChecklistItem(
                permission: permission,
                title: value,
                detail: "Open System Settings and enable Polarize.",
                graphicIconUTI: nil,
                symbolName: "questionmark.circle"
            )
        }
    }
}
