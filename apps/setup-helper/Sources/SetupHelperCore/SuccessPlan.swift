// Pure success-state logic for PLZ-9: the helper's SIGUSR1-driven
// close. No AppKit import, no live `DispatchSource`/`exit` call — the
// seam for those is `main.swift`, so this file stays testable in plain
// `swift test`. See PINV-58, PINV-64, and PINV-65 in
// `docs/INVARIANTS.md` for the rules this success state must not
// break: it reads no permission API, it never races the parent's own
// `SIGKILL`, and it never speaks for a status the parent did not
// already decide.
import Foundation

/// What the helper shows and does once `SIGUSR1` arrives — the
/// parent's own signal that its read says every requested permission
/// is now granted (PLZ-9). The helper never reads a permission API of
/// its own to reach this state; it only reacts to the parent.
public enum SuccessPlan {
    /// How long the helper waits, after swapping in the success view,
    /// before it calls `exit(0)`.
    ///
    /// Cross-language coupling: `polarize_core::bootstrap::GRANT_SUCCESS_GRACE_MS`
    /// (`crates/polarize-core/src/bootstrap.rs`), the parent's own
    /// grace period before it sends `SIGKILL`, must stay comfortably
    /// above this value — this delay only has a chance to run to
    /// completion inside that window. No shared constant enforces
    /// this; only the comment on each side. 0.8s here against the
    /// parent's 1.5s leaves real margin.
    public static let quitDelaySeconds: Double = 0.8

    /// The success view's own text. Phrased as a fact the parent
    /// reported, never as something the helper itself determined — the
    /// helper has no way to read Polarize's own grant state (PINV-56).
    public static let message = "Polarize has what it needs. You can close System Settings."
}
