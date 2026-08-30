// Resolves a checklist row's real icon. This is the exact artwork
// System Settings' own Privacy & Security pane uses. The API path is
// fully public: `UTType(_:)` plus `NSWorkspace.shared.icon(for:)`,
// against the `com.apple.graphic-icon.*` UTIs System Settings' own
// Security & Privacy extension declares. Confirmed live against a
// real macOS 27 session: this produces icons pixel-identical to what
// System Settings itself shows. Examples: the blue accessibility
// figure, the red record-dot screen-recording icon, and the two-gear
// automation icon. No private framework is involved. No path reaches
// into System Settings' own app bundle either. This falls back to an
// SF Symbol (`SetupHelperCore.PermissionChecklistItem.symbolName`)
// whenever the UTI isn't declared on the current macOS version, or
// there is no `graphicIconUTI` at all (`.unknown`).
import AppKit
import SetupHelperCore
import UniformTypeIdentifiers

enum PermissionIcon {
    /// `image` is the icon to show. `isSymbol` tells the caller
    /// whether tinting it is safe. The real System Settings artwork is
    /// a full-color, pre-rendered icon. It must never be tinted. The
    /// SF Symbol fallback is a template image instead. It should pick
    /// up `.controlAccentColor`, like any other symbol in this UI.
    struct Resolved {
        let image: NSImage
        let isSymbol: Bool
    }

    static func resolve(for item: PermissionChecklistItem) -> Resolved {
        if let uti = item.graphicIconUTI, let type = UTType(uti), type.isDeclared {
            return Resolved(image: NSWorkspace.shared.icon(for: type), isSymbol: false)
        }
        let symbol = NSImage(systemSymbolName: item.symbolName, accessibilityDescription: item.title) ?? NSImage()
        return Resolved(image: symbol, isSymbol: true)
    }
}
