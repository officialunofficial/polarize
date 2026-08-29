// Tests for `SetupHelperCore`'s pure PLZ-9 success-state constants. No
// AppKit, no live `DispatchSource`/`exit` call — see PINV-65's
// checklist entry in `docs/INVARIANTS.md` for the cross-language
// timing coupling this file's `quitDelaySeconds` constant is one half
// of.
import XCTest

@testable import SetupHelperCore

final class SuccessPlanTests: XCTestCase {

    func testQuitDelayStaysWellUnderTheParentsGraceWindow() {
        // The parent's own grace period is
        // `polarize_core::bootstrap::GRANT_SUCCESS_GRACE_MS = 1_500`ms
        // (`crates/polarize-core/src/bootstrap.rs`). This assertion is
        // this file's half of that cross-language coupling: it fails
        // loudly if a future edit ever pushes the Swift delay close to,
        // or past, the Rust grace period.
        let parentGraceSeconds = 1.5
        XCTAssertLessThan(SuccessPlan.quitDelaySeconds, parentGraceSeconds)
        XCTAssertGreaterThan(SuccessPlan.quitDelaySeconds, 0)
    }

    func testMessageIsNonEmptyAndNeverSpeaksOfAPermissionCheckOfItsOwn() {
        XCTAssertFalse(SuccessPlan.message.isEmpty)
        // The helper reads no permission API of its own (PINV-56,
        // PINV-58) — its success text must never claim to have checked
        // anything itself.
        let lower = SuccessPlan.message.lowercased()
        XCTAssertFalse(lower.contains("checked"))
        XCTAssertFalse(lower.contains("verified"))
    }
}
