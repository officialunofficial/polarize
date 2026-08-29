// Tests for `SetupHelperCore`'s drag-payload logic: parsing the
// `--for-bundle` argv flag, building the pure pasteboard payload for
// Polarize's own bundle icon, and deciding when the helper's drag view
// may show at all. No AppKit, no live drag session — see PINV-59 and
// PINV-60's checklist entries in `docs/INVARIANTS.md` for what these
// tests cover and what still needs a live macOS session.
import XCTest

@testable import SetupHelperCore

final class DragPayloadTests: XCTestCase {

    // MARK: - ArgvParser.bundlePath

    func testBundlePathIsNilForEmptyArgv() {
        XCTAssertNil(ArgvParser.bundlePath([]))
    }

    func testBundlePathReadsTheForBundleValue() {
        XCTAssertEqual(
            ArgvParser.bundlePath(["--for-bundle", "/Applications/Polarize.app"]),
            "/Applications/Polarize.app"
        )
    }

    func testBundlePathReadsItInterleavedWithNeedsFlags() {
        XCTAssertEqual(
            ArgvParser.bundlePath([
                "--needs", "accessibility",
                "--for-bundle", "/Applications/Polarize.app",
                "--needs", "screen-recording",
            ]),
            "/Applications/Polarize.app"
        )
    }

    func testBundlePathIsNilForATrailingForBundleFlagWithNoValue() {
        XCTAssertNil(ArgvParser.bundlePath(["--needs", "accessibility", "--for-bundle"]))
    }

    // MARK: - DragPayload (PINV-59)

    func testInitSucceedsForPolarizeAppBundlePath() {
        XCTAssertNotNil(DragPayload(bundlePath: "/Applications/Polarize.app"))
    }

    func testInitFailsWhenThePathDoesNotEndInDotApp() {
        XCTAssertNil(DragPayload(bundlePath: "/Applications/Polarize"))
    }

    func testInitFailsForTheHelpersOwnBundle() {
        XCTAssertNil(
            DragPayload(
                bundlePath:
                    "/Applications/Polarize.app/Contents/Resources/PolarizeSetupHelper.app"
            )
        )
    }

    func testInitFailsForAPathInsideTheHelpersOwnBundle() {
        XCTAssertNil(
            DragPayload(
                bundlePath:
                    "/Applications/Polarize.app/Contents/Resources/PolarizeSetupHelper.app/Contents/MacOS/PolarizeSetupHelper"
            )
        )
    }

    func testEveryRepresentationResolvesToTheGivenBundlePath() throws {
        let path = "/Applications/Polarize.app"
        let payload = try XCTUnwrap(DragPayload(bundlePath: path))

        let rawTypes = payload.representations.map(\.rawType)
        XCTAssertEqual(
            Set(rawTypes),
            ["public.file-url", "public.url", "NSFilenamesPboardType", "public.utf8-plain-text"]
        )
        XCTAssertEqual(rawTypes.count, 4, "each pasteboard type must appear exactly once")

        for representation in payload.representations {
            switch (representation.rawType, representation.value) {
            case ("public.file-url", .string(let urlString)),
                ("public.url", .string(let urlString)):
                XCTAssertEqual(URL(string: urlString)?.path, path)
            case ("NSFilenamesPboardType", .stringArray(let paths)):
                XCTAssertEqual(paths, [path])
            case ("public.utf8-plain-text", .string(let plain)):
                XCTAssertEqual(plain, path)
            default:
                XCTFail("unexpected representation \(representation.rawType) / \(representation.value)")
            }
        }
    }

    // MARK: - DragSourcePlanner (PINV-60)

    func testPayloadIsNilWhenOutcomeIsNothingToOpen() {
        XCTAssertNil(
            DragSourcePlanner.payload(outcome: .nothingToOpen, bundlePath: "/Applications/Polarize.app")
        )
    }

    func testPayloadIsNilWhenBundlePathIsNil() {
        XCTAssertNil(
            DragSourcePlanner.payload(
                outcome: .openedPane(
                    permission: .accessibility,
                    urlString: SettingsPane.urlString(for: .accessibility)!
                ),
                bundlePath: nil
            )
        )
    }

    func testPayloadIsPresentForAnOpenedAccessibilityPane() {
        let outcome = LaunchOutcome.openedPane(
            permission: .accessibility,
            urlString: SettingsPane.urlString(for: .accessibility)!
        )
        XCTAssertNotNil(
            DragSourcePlanner.payload(outcome: outcome, bundlePath: "/Applications/Polarize.app")
        )
    }

    func testPayloadIsPresentForAnOpenedScreenRecordingPane() {
        let outcome = LaunchOutcome.openedPane(
            permission: .screenRecording,
            urlString: SettingsPane.urlString(for: .screenRecording)!
        )
        XCTAssertNotNil(
            DragSourcePlanner.payload(outcome: outcome, bundlePath: "/Applications/Polarize.app")
        )
    }

    func testPayloadIsPresentEvenOnTheFallbackPathForAMappedPermission() {
        let outcome = LaunchOutcome.fellBackToInstructions(
            permission: .screenRecording,
            fallbackURLString: SettingsPane.fallbackURLString
        )
        XCTAssertNotNil(
            DragSourcePlanner.payload(outcome: outcome, bundlePath: "/Applications/Polarize.app")
        )
    }

    // PLZ-10: since `SettingsPane.urlString` now maps Automation to a
    // real anchor, `.openedPane`/`.fellBackToInstructions` alone no
    // longer imply a draggable permission. This is the direct
    // regression test for the explicit rule in `DragSourcePlanner`:
    // even with a real, non-nil Automation pane URL, no drag payload is
    // offered — Automation's only grant mechanism is a live Apple Event
    // send, never a drag.
    func testPayloadIsNilForAnOpenedAutomationPaneEvenWithARealURL() {
        let outcome = LaunchOutcome.openedPane(
            permission: .automation(target: "Finder"),
            urlString: SettingsPane.automationURLString
        )
        XCTAssertNil(
            DragSourcePlanner.payload(outcome: outcome, bundlePath: "/Applications/Polarize.app")
        )
    }

    func testPayloadIsNilForAFallenBackAutomationPaneEvenWithARealURL() {
        let outcome = LaunchOutcome.fellBackToInstructions(
            permission: .automation(target: "Finder"),
            fallbackURLString: SettingsPane.fallbackURLString
        )
        XCTAssertNil(
            DragSourcePlanner.payload(outcome: outcome, bundlePath: "/Applications/Polarize.app")
        )
    }

    func testPayloadIsNilWhenTheBundlePathIsTheHelpersOwnEvenIfOutcomeIsMapped() {
        let outcome = LaunchOutcome.openedPane(
            permission: .accessibility,
            urlString: SettingsPane.urlString(for: .accessibility)!
        )
        XCTAssertNil(
            DragSourcePlanner.payload(
                outcome: outcome,
                bundlePath: "/Applications/Polarize.app/Contents/Resources/PolarizeSetupHelper.app"
            )
        )
    }

    // Table-driven over every `NeededPermission` case an outcome could
    // name, run through `LaunchPlanner` first exactly as the helper
    // does. Since PLZ-10, Automation opens a real pane like Accessibility
    // and Screen Recording do, so `DragSourcePlanner` must reject it by
    // its explicit `supportsDragGrant` rule, not by `LaunchOutcome`
    // shape — an empty set or an unknown-only set still yields
    // `.nothingToOpen`, but an automation-only or automation-first set
    // now yields `.openedPane` with no payload.
    func testDragPayloadNonNilOnlyForAccessibilityOrScreenRecordingOutcomes() {
        let cases: [(needed: [NeededPermission], expectPayload: Bool)] = [
            ([.accessibility], true),
            ([.screenRecording], true),
            ([.automation(target: "Finder")], false),
            ([], false),
            ([.unknown("some-future-permission")], false),
            ([.automation(target: "Finder"), .accessibility], false),
            ([.accessibility, .automation(target: "Finder")], true),
        ]

        for testCase in cases {
            let outcome = LaunchPlanner.run(needed: testCase.needed) { _ in true }
            let payload = DragSourcePlanner.payload(
                outcome: outcome,
                bundlePath: "/Applications/Polarize.app"
            )
            XCTAssertEqual(
                payload != nil,
                testCase.expectPayload,
                "needed=\(testCase.needed) outcome=\(outcome)"
            )
        }
    }
}
