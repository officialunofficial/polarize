// Pure window-tracking logic for the guided setup helper's floating
// pane: picking System Settings' own window out of a raw window list,
// planning the pane's on-screen frame so it follows that window, and
// choosing which tracking mechanism to use. No AppKit import, no live
// `CGWindowListCopyWindowInfo` or `AXObserver` call — those live in
// `PolarizeSetupHelper/main.swift`, so this file stays testable in
// plain `swift test`. See PINV-62 in `docs/INVARIANTS.md`.
import Foundation
import CoreGraphics

/// A pure mirror of one `CGWindowListCopyWindowInfo` entry, holding
/// only the keys the locator needs.
///
/// Deliberately carries no title field. macOS hides `kCGWindowName`
/// from a process without Screen Recording (see
/// `crates/polarize-macos/src/workspace.rs`'s module doc comment for
/// the same fact on the Rust side), and the helper has no Screen
/// Recording of its own — so the locator must never depend on a
/// window's title to find System Settings.
public struct TrackedWindowInfo: Equatable {
    public let ownerPID: Int
    public let layer: Int
    public let isOnScreen: Bool
    public let bounds: CGRect

    public init(ownerPID: Int, layer: Int, isOnScreen: Bool, bounds: CGRect) {
        self.ownerPID = ownerPID
        self.layer = layer
        self.isOnScreen = isOnScreen
        self.bounds = bounds
    }
}

/// Picks System Settings' own on-screen window out of a raw window
/// list.
///
/// Matches by owner process id, never by title — see
/// `TrackedWindowInfo`'s own doc comment for why. `layer == 0` and
/// `isOnScreen` keep the match to an ordinary, visible window; System
/// Settings can own small auxiliary windows (menu extras, tooltips)
/// that also sit at layer 0, so among every match the locator picks
/// the one with the largest area.
public enum SettingsWindowLocator {
    public static func pick(
        from windows: [TrackedWindowInfo],
        ownedByPIDs: Set<Int>
    ) -> TrackedWindowInfo? {
        windows
            .filter { ownedByPIDs.contains($0.ownerPID) && $0.layer == 0 && $0.isOnScreen }
            .max { area($0.bounds) < area($1.bounds) }
    }

    private static func area(_ rect: CGRect) -> CGFloat {
        rect.size.width * rect.size.height
    }
}

/// Plans the floating panel's on-screen frame from System Settings'
/// own window bounds.
///
/// `CGWindowListCopyWindowInfo` reports bounds in Quartz's top-left-
/// origin, y-down coordinate system. `NSWindow`/`NSPanel` frames use
/// AppKit's bottom-left-origin, y-up system. Converting the same
/// physical point between the two flips y:
/// `cocoaY = screenHeight - cgY - height`. x needs no flip — both
/// systems measure it left-to-right from the same origin.
///
/// Placement policy: the panel sits at System Settings' trailing
/// (right) edge, top-aligned, offset by `edgeMargin` — never on top of
/// the window it tracks, so the user can still see and use both.
public enum PanelFramePlanner {
    /// The gap, in points, between System Settings' trailing edge and
    /// the panel's leading edge.
    public static let edgeMargin: CGFloat = 12

    public static func frame(
        overSettingsBounds bounds: CGRect,
        screenHeight: CGFloat,
        panelSize: CGSize
    ) -> CGRect {
        let x = bounds.origin.x + bounds.size.width + edgeMargin
        let y = screenHeight - bounds.origin.y - panelSize.height
        return CGRect(origin: CGPoint(x: x, y: y), size: panelSize)
    }

    /// The panel's frame when no System Settings window can be found —
    /// an automation-only launch that never opened Settings, or the
    /// user quit Settings mid-flow. Centers the panel on the given
    /// screen rather than hiding it, so there is always a visible
    /// window with a path forward (PINV-63's same spirit).
    public static func fallbackFrame(
        screenWidth: CGFloat,
        screenHeight: CGFloat,
        panelSize: CGSize
    ) -> CGRect {
        let x = (screenWidth - panelSize.width) / 2
        let y = (screenHeight - panelSize.height) / 2
        return CGRect(origin: CGPoint(x: x, y: y), size: panelSize)
    }
}

/// Which mechanism the helper uses to keep its panel tracking System
/// Settings' window.
public enum TrackingStrategy: Equatable {
    /// Poll `CGWindowListCopyWindowInfo` on a timer. Needs no
    /// permission of any kind — see PINV-62.
    case cgWindowPolling
    /// Register an `AXObserver` on System Settings' window element for
    /// move/resize notifications. Needs the *helper's own* process to
    /// already hold Accessibility trust, which this feature has no way
    /// to grant (see the PLZ-7 plan's risk note 2) — so this branch is
    /// structurally correct but rarely, if ever, reachable in practice.
    case axObserver
}

/// Chooses a `TrackingStrategy` once, at helper launch.
///
/// `helperIsAXTrusted` must come from a single, non-prompting
/// `AXIsProcessTrusted()` read of the helper's own process — never a
/// prompting variant, and never presented as Polarize's own grant
/// state (PINV-56, PINV-58).
public enum TrackingStrategyPicker {
    public static func pick(helperIsAXTrusted: Bool) -> TrackingStrategy {
        helperIsAXTrusted ? .axObserver : .cgWindowPolling
    }
}

/// System Settings' own bundle identifier, used to resolve its running
/// process id(s) via `NSRunningApplication`.
///
/// Verified by a read-only `Info.plist` inspection of
/// `/System/Applications/System Settings.app` on macOS 27.0
/// (`CFBundleIdentifier = com.apple.systempreferences`, lowercase).
/// Confirmed against macOS 26/27-era System Settings only; earlier
/// macOS versions (where the app was still named System Preferences)
/// are not independently re-verified here, though this identifier has
/// historically stayed stable across the rename.
public enum SystemSettings {
    public static let bundleIdentifier = "com.apple.systempreferences"
}
