// Pure drag-payload logic for the guided setup helper's "drag me"
// affordance (PLZ-8): parsing the `--for-bundle` argv flag the Rust
// side adds (`polarize_core::bootstrap::helper_args`), building the
// pasteboard representations a Finder-style drag needs, and deciding
// when the drag view may show at all. No AppKit import — raw UTI
// strings only, since `NSPasteboard.PasteboardType` lives in AppKit
// and the pasteboard-writing glue belongs in `PolarizeSetupHelper`
// instead. See PINV-59 and PINV-60 in `docs/INVARIANTS.md`.
import Foundation

extension ArgvParser {
    /// Reads the last `--for-bundle <value>` pair out of the helper's
    /// launch argv — the path to Polarize's own running bundle,
    /// resolved on the Rust side by
    /// `polarize_macos::setup_helper::own_bundle_path` and passed
    /// through `polarize_core::bootstrap::helper_args`. `nil` when the
    /// flag is absent (an unbundled dev run) or has no following
    /// value.
    public static func bundlePath(_ arguments: [String]) -> String? {
        var index = 0
        var found: String?
        while index < arguments.count {
            if arguments[index] == "--for-bundle", index + 1 < arguments.count {
                found = arguments[index + 1]
                index += 2
            } else {
                index += 1
            }
        }
        return found
    }
}

/// The pure form of one pasteboard representation's value: either a
/// single string, or an array of strings (the legacy
/// `NSFilenamesPboardType` shape, a property-list array under the
/// hood). Modeled as an enum, rather than handing AppKit a raw `Any`,
/// so the AppKit glue that reads it can never mis-decode which shape a
/// given raw type carries.
public enum PasteboardValue: Equatable {
    case string(String)
    case stringArray([String])
}

/// The drag payload for one app bundle: every pasteboard representation
/// a Finder-style icon drag offers, all resolved from the same
/// `bundlePath` this payload was built with.
///
/// # PINV-59
///
/// `init?` only ever succeeds for a path that plausibly names an app
/// bundle other than the helper's own — it is the structural guard
/// behind "the drag source always names Polarize's own bundle, never
/// the helper's." A caller still must pass the *right* path in the
/// first place (see `DragSourcePlanner`); this initializer only
/// rejects an obviously wrong one.
public struct DragPayload: Equatable {
    /// One pasteboard representation: a raw UTI (or legacy pasteboard
    /// type) string, paired with its value.
    public struct Representation: Equatable {
        public let rawType: String
        public let value: PasteboardValue
    }

    /// Every pasteboard representation, in order. Each of the four
    /// Finder-drag shapes appears exactly once.
    public let representations: [Representation]

    /// Rejects a path that does not end in `.app`, and rejects any
    /// path at or inside a bundle literally named
    /// `PolarizeSetupHelper.app` — the one bundle identity this drag
    /// must never name (PINV-59).
    public init?(bundlePath: String) {
        guard bundlePath.hasSuffix(".app") else { return nil }
        let components = bundlePath.split(separator: "/")
        guard !components.contains("PolarizeSetupHelper.app") else { return nil }

        representations = [
            Representation(rawType: "public.file-url", value: .string(Self.fileURLString(for: bundlePath))),
            Representation(rawType: "public.url", value: .string(Self.fileURLString(for: bundlePath))),
            Representation(rawType: "NSFilenamesPboardType", value: .stringArray([bundlePath])),
            Representation(rawType: "public.utf8-plain-text", value: .string(bundlePath)),
        ]
    }

    private static func fileURLString(for path: String) -> String {
        URL(fileURLWithPath: path, isDirectory: true).absoluteString
    }
}

/// Decides whether the helper's drag view may show at all, and with
/// what payload.
///
/// # PINV-60
///
/// Since PLZ-10, `SettingsPane.urlString` maps Automation to a real
/// pane, so `LaunchOutcome` alone no longer distinguishes "a draggable
/// permission's pane opened" from "the Automation pane opened" — both
/// are `.openedPane` (or `.fellBackToInstructions`). Automation's grant
/// mechanism is still a live Apple Event send, not a drag, so this is
/// now an explicit rule rather than a composed one: switch on the
/// outcome's own `permission` and return `nil` for `.automation` (and
/// `.unknown`, unreachable here but named for completeness) via
/// `NeededPermission.supportsDragGrant`. Never remove this switch to
/// "just check `.openedPane`" again — that would silently re-offer a
/// drag affordance for a permission a drag can never grant.
public enum DragSourcePlanner {
    public static func payload(outcome: LaunchOutcome, bundlePath: String?) -> DragPayload? {
        switch outcome {
        case .nothingToOpen:
            return nil
        case .openedPane(let permission, _), .fellBackToInstructions(let permission, _):
            guard permission.supportsDragGrant, let bundlePath else { return nil }
            return DragPayload(bundlePath: bundlePath)
        }
    }
}

extension NeededPermission {
    /// Whether dragging Polarize's own app icon into this permission's
    /// System Settings row can grant it. `true` only for Accessibility
    /// and Screen Recording — Finder-style drag-to-list is a real macOS
    /// TCC affordance for those two panes. `false` for Automation, whose
    /// only grant mechanism is a live Apple Event send (PINV-57), and
    /// for `.unknown`, which names no pane at all.
    var supportsDragGrant: Bool {
        switch self {
        case .accessibility, .screenRecording:
            return true
        case .automation, .unknown:
            return false
        }
    }
}
