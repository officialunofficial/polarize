// Tests for `SetupHelperCore`'s pure permission-pane logic: the argv
// parser, the deep-link pane mapping table, and the fallback-aware
// launch plan. No AppKit, no live System Settings — see PINV-63's
// checklist entry in `docs/INVARIANTS.md` for what these tests cover
// and what still needs a live macOS session.
import XCTest

@testable import SetupHelperCore

final class PermissionPaneTests: XCTestCase {

    // MARK: - ArgvParser

    func testParseIsEmptyForEmptyArgv() {
        XCTAssertEqual(ArgvParser.parse([]), [])
    }

    func testParseReadsAccessibility() {
        XCTAssertEqual(
            ArgvParser.parse(["--needs", "accessibility"]),
            [.accessibility]
        )
    }

    func testParseReadsScreenRecording() {
        XCTAssertEqual(
            ArgvParser.parse(["--needs", "screen-recording"]),
            [.screenRecording]
        )
    }

    func testParseReadsAutomationWithItsTarget() {
        XCTAssertEqual(
            ArgvParser.parse(["--needs", "automation:Finder"]),
            [.automation(target: "Finder")]
        )
    }

    func testParseReadsEveryNeedsFlagInOrder() {
        XCTAssertEqual(
            ArgvParser.parse([
                "--needs", "accessibility",
                "--needs", "screen-recording",
                "--needs", "automation:Mail",
            ]),
            [.accessibility, .screenRecording, .automation(target: "Mail")]
        )
    }

    func testParseCarriesAnUnknownValueRatherThanCrashing() {
        XCTAssertEqual(
            ArgvParser.parse(["--needs", "some-future-permission"]),
            [.unknown("some-future-permission")]
        )
    }

    func testParseIgnoresArgumentsOutsideAKnownNeedsPair() {
        XCTAssertEqual(
            ArgvParser.parse(["--verbose", "--needs", "accessibility", "--other", "value"]),
            [.accessibility]
        )
    }

    func testParseIgnoresATrailingNeedsFlagWithNoValue() {
        XCTAssertEqual(ArgvParser.parse(["--needs"]), [])
    }

    // MARK: - SettingsPane mapping

    func testAccessibilityMapsToItsExactAnchor() {
        XCTAssertEqual(
            SettingsPane.urlString(for: .accessibility),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        )
    }

    func testScreenRecordingMapsToItsExactAnchor() {
        XCTAssertEqual(
            SettingsPane.urlString(for: .screenRecording),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        )
    }

    func testAutomationMapsToItsExactAnchor() {
        XCTAssertEqual(
            SettingsPane.urlString(for: .automation(target: "Finder")),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"
        )
    }

    func testAutomationMapsToTheSameAnchorRegardlessOfTarget() {
        XCTAssertEqual(
            SettingsPane.urlString(for: .automation(target: "Finder")),
            SettingsPane.urlString(for: .automation(target: "Mail"))
        )
    }

    func testUnknownMapsToNoPane() {
        XCTAssertNil(SettingsPane.urlString(for: .unknown("some-future-permission")))
    }

    func testFallbackURLStringIsTheTopLevelPrivacyAndSecurityPane() {
        XCTAssertEqual(
            SettingsPane.fallbackURLString,
            "x-apple.systempreferences:com.apple.preference.security"
        )
    }

    // MARK: - LaunchPlanner

    func testAccessibilityAloneOpensTheAccessibilityPane() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: [.accessibility]) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.urlString(for: .accessibility)!])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .accessibility, urlString: SettingsPane.urlString(for: .accessibility)!)
        )
    }

    func testScreenRecordingAloneOpensTheScreenRecordingPane() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: [.screenRecording]) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.urlString(for: .screenRecording)!])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .screenRecording, urlString: SettingsPane.urlString(for: .screenRecording)!)
        )
    }

    func testOpenReturningFalseTriggersExactlyOneFallbackAttempt() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: [.accessibility]) { urlString in
            opened.append(urlString)
            return false
        }
        XCTAssertEqual(
            opened,
            [SettingsPane.urlString(for: .accessibility)!, SettingsPane.fallbackURLString]
        )
        XCTAssertEqual(
            outcome,
            .fellBackToInstructions(permission: .accessibility, fallbackURLString: SettingsPane.fallbackURLString)
        )
    }

    func testAutomationOnlyLaunchOpensTheAutomationPane() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: [.automation(target: "Finder")]) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.automationURLString])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .automation(target: "Finder"), urlString: SettingsPane.automationURLString)
        )
    }

    func testAutomationAnchorFailingToOpenTriggersExactlyOneFallbackAttempt() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: [.automation(target: "Finder")]) { urlString in
            opened.append(urlString)
            return false
        }
        XCTAssertEqual(opened, [SettingsPane.automationURLString, SettingsPane.fallbackURLString])
        XCTAssertEqual(
            outcome,
            .fellBackToInstructions(
                permission: .automation(target: "Finder"),
                fallbackURLString: SettingsPane.fallbackURLString
            )
        )
    }

    func testEmptyNeededLaunchCallsOpenZeroTimes() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(needed: []) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [])
        XCTAssertEqual(outcome, .nothingToOpen)
    }

    func testMultiPermissionLaunchOpensOnlyTheFirstMappedPane() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(
            needed: [.accessibility, .screenRecording, .automation(target: "Mail")]
        ) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.urlString(for: .accessibility)!])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .accessibility, urlString: SettingsPane.urlString(for: .accessibility)!)
        )
    }

    // Since PLZ-10, Automation maps to a real pane, so it no longer
    // stands in for "unmapped" — only `.unknown` does. This test now
    // exercises that case directly.
    func testAnUnmappedPermissionBeforeAMappedOneIsSkippedOverNotOpened() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(
            needed: [.unknown("some-future-permission"), .screenRecording]
        ) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.urlString(for: .screenRecording)!])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .screenRecording, urlString: SettingsPane.urlString(for: .screenRecording)!)
        )
    }

    // PLZ-10: with the Rust side's fixed emission order (Accessibility,
    // Screen Recording, Automation — see `PermissionPane.swift`'s
    // `LaunchPlanner` doc comment), Automation before a draggable
    // permission opens the Automation pane, not the draggable one. This
    // pins today's strict-argv-order behavior; open question 1 in
    // `docs/INVARIANTS.md` covers whether that should change.
    func testAutomationBeforeADraggablePermissionOpensTheAutomationPaneUnderStrictArgvOrder() {
        var opened: [String] = []
        let outcome = LaunchPlanner.run(
            needed: [.automation(target: "Mail"), .screenRecording]
        ) { urlString in
            opened.append(urlString)
            return true
        }
        XCTAssertEqual(opened, [SettingsPane.automationURLString])
        XCTAssertEqual(
            outcome,
            .openedPane(permission: .automation(target: "Mail"), urlString: SettingsPane.automationURLString)
        )
    }
}
