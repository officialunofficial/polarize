// Pure permission-pane logic for the guided setup helper: parsing the
// `--needs` argv the Rust side builds (`polarize_core::bootstrap::helper_args`),
// mapping each permission to its System Settings deep link, and
// choosing which pane to open when the helper launches. No AppKit
// import, no live System Settings call — the seam for that is the
// `open` closure `LaunchPlanner.run` takes, so this file stays testable
// in plain `swift test`. See PINV-58 through PINV-63 in
// `docs/INVARIANTS.md`.
import Foundation

/// One permission the helper's argv can name, mirroring
/// `polarize_core::bootstrap::NeededPermission` on the Rust side.
/// `.unknown` holds any `--needs` value this build does not recognize,
/// so a future Rust-side addition never crashes an older helper — it
/// is simply skipped when choosing a pane.
public enum NeededPermission: Equatable {
    case accessibility
    case screenRecording
    case automation(target: String)
    case unknown(String)
}

/// Parses the helper's launch argv into the permissions it names.
///
/// Reads `--needs <value>` pairs in order, exactly as
/// `polarize_core::bootstrap::helper_args` emits them:
/// `"accessibility"`, `"screen-recording"`, or `"automation:<target>"`.
/// A `--needs` flag with no following value, or any other argument, is
/// ignored rather than treated as an error — this parser only ever
/// reads `--needs` pairs, skipping past every other flag the argv
/// carries (such as `--for-bundle`, read separately by
/// `ArgvParser.bundlePath` in `DragPayload.swift`), so an unrecognized
/// shape never crashes it.
public enum ArgvParser {
    public static func parse(_ arguments: [String]) -> [NeededPermission] {
        var needed: [NeededPermission] = []
        var index = 0
        while index < arguments.count {
            if arguments[index] == "--needs", index + 1 < arguments.count {
                needed.append(permission(fromValue: arguments[index + 1]))
                index += 2
            } else {
                index += 1
            }
        }
        return needed
    }

    private static func permission(fromValue value: String) -> NeededPermission {
        switch value {
        case "accessibility":
            return .accessibility
        case "screen-recording":
            return .screenRecording
        default:
            if let target = value.stripPrefix("automation:") {
                return .automation(target: target)
            }
            return .unknown(value)
        }
    }
}

extension String {
    fileprivate func stripPrefix(_ prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }
}

/// Maps each permission to its System Settings deep link.
///
/// Automation has no pane mapping in this slice — its real grant
/// mechanism is a live Apple Event send (PINV-60), not a settings pane
/// — so `urlString(for:)` returns `nil` for it, and the helper skips
/// deep-linking for an automation-only launch entirely.
public enum SettingsPane {
    /// The top-level Privacy & Security pane, opened when a mapped
    /// permission's own anchor fails to resolve (PINV-63).
    public static let fallbackURLString = "x-apple.systempreferences:com.apple.preference.security"

    public static func urlString(for permission: NeededPermission) -> String? {
        switch permission {
        case .accessibility:
            return "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        case .screenRecording:
            return "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        case .automation, .unknown:
            return nil
        }
    }
}

/// What the helper did when it tried to open a pane for the permission
/// set the Rust side named.
public enum LaunchOutcome: Equatable {
    /// The mapped pane opened on the first attempt.
    case openedPane(permission: NeededPermission, urlString: String)
    /// The mapped pane's own anchor failed to open (`open` returned
    /// `false`), so the helper opened the fallback pane instead
    /// (PINV-63). The window must still show plain instructions — see
    /// `main.swift`.
    case fellBackToInstructions(permission: NeededPermission, fallbackURLString: String)
    /// No permission in the set maps to a pane — an automation-only
    /// launch, or an empty set. The helper opens nothing and shows its
    /// plain window (PLZ-4 behavior).
    case nothingToOpen
}

/// Chooses which pane to open, given the permissions the helper was
/// launched to cover.
///
/// Picks the first permission in argv order that maps to a pane, and
/// opens only that one — System Settings can show only one pane at a
/// time, so a multi-permission launch does not attempt every mapped
/// permission in turn (see `docs/INVARIANTS.md`'s PLZ-6 risk note on
/// this exact choice). `open` is the seam for `NSWorkspace.shared.open`;
/// it is never called more than twice — once for the mapped pane, and
/// once more for the fallback pane only if the first call reports
/// failure.
public enum LaunchPlanner {
    public static func run(
        needed: [NeededPermission],
        open: (String) -> Bool
    ) -> LaunchOutcome {
        guard
            let permission = needed.first(where: { SettingsPane.urlString(for: $0) != nil }),
            let urlString = SettingsPane.urlString(for: permission)
        else {
            return .nothingToOpen
        }

        if open(urlString) {
            return .openedPane(permission: permission, urlString: urlString)
        }

        _ = open(SettingsPane.fallbackURLString)
        return .fellBackToInstructions(permission: permission, fallbackURLString: SettingsPane.fallbackURLString)
    }
}
