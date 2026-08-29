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
/// Composed entirely from existing logic — `LaunchOutcome` already
/// carries `.nothingToOpen` for an automation-only or empty launch,
/// because `SettingsPane.urlString` already returns `nil` for
/// Automation and unknown permissions. No Automation-specific branch
/// lives here: the drag view is offered only when `LaunchPlanner`
/// actually mapped the launch to a pane (`.openedPane` or
/// `.fellBackToInstructions`), which by construction only ever happens
/// for Accessibility or Screen Recording.
public enum DragSourcePlanner {
    public static func payload(outcome: LaunchOutcome, bundlePath: String?) -> DragPayload? {
        switch outcome {
        case .nothingToOpen:
            return nil
        case .openedPane, .fellBackToInstructions:
            guard let bundlePath else { return nil }
            return DragPayload(bundlePath: bundlePath)
        }
    }
}
