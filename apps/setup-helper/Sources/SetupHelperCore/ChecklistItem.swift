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
    /// One row per `needed` permission, in the same order. `osMajorVersion`
    /// picks each row's title from the real pane name on that OS — never
    /// hardcoded to one version, and never read from live system state
    /// inside this pure module. The caller supplies it, normally from
    /// `ProcessInfo.processInfo.operatingSystemVersion.majorVersion`, so
    /// a test can supply any version instead.
    public static func items(for needed: [NeededPermission], osMajorVersion: Int) -> [PermissionChecklistItem] {
        needed.map { item(for: $0, osMajorVersion: osMajorVersion) }
    }

    public static func item(for permission: NeededPermission, osMajorVersion: Int) -> PermissionChecklistItem {
        switch permission {
        case .accessibility:
            // Confirmed live: renamed "Device Control and Data Access"
            // starting macOS 27 beta 5. Still "Accessibility" on every
            // version before that, including macOS 26 Tahoe.
            return PermissionChecklistItem(
                permission: permission,
                title: osMajorVersion >= 27 ? "Device Control and Data Access" : "Accessibility",
                detail: "Allows Polarize to see and control other apps' interfaces.",
                graphicIconUTI: "com.apple.graphic-icon.accessibility",
                symbolName: "accessibility"
            )
        case .screenRecording:
            // Renamed "Screen & System Audio Recording" starting macOS
            // 15 Sequoia (support.apple.com/guide/mac-help/mchld6aa7d23).
            return PermissionChecklistItem(
                permission: permission,
                title: osMajorVersion >= 15 ? "Screen & System Audio Recording" : "Screen Recording",
                detail: "Polarize captures screenshots to see what is on screen.",
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
