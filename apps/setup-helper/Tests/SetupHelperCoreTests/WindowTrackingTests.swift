// Tests for `SetupHelperCore`'s pure window-tracking logic: the
// System Settings window locator, the panel frame planner's
// CG-to-Cocoa y-flip, and the tracking strategy picker. No AppKit, no
// live `CGWindowListCopyWindowInfo` call — see PINV-62's checklist
// entry in `docs/INVARIANTS.md` for what these tests cover and what
// still needs a live macOS session.
import XCTest

@testable import SetupHelperCore

final class WindowTrackingTests: XCTestCase {

    // MARK: - SettingsWindowLocator

    func testLocatorPicksTheOnlyMatchingWindow() {
        let settings = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        let picked = SettingsWindowLocator.pick(from: [settings], ownedByPIDs: [100])
        XCTAssertEqual(picked, settings)
    }

    func testLocatorSkipsAWindowOwnedByAnUnrelatedPID() {
        let decoy = TrackedWindowInfo(
            ownerPID: 999, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        XCTAssertNil(SettingsWindowLocator.pick(from: [decoy], ownedByPIDs: [100]))
    }

    func testLocatorSkipsAHigherLayerWindow() {
        let decoy = TrackedWindowInfo(
            ownerPID: 100, layer: 3, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        XCTAssertNil(SettingsWindowLocator.pick(from: [decoy], ownedByPIDs: [100]))
    }

    func testLocatorSkipsAnOffScreenWindow() {
        let decoy = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: false,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        XCTAssertNil(SettingsWindowLocator.pick(from: [decoy], ownedByPIDs: [100]))
    }

    func testLocatorPicksTheLargestAreaAmongSeveralMatches() {
        let auxiliary = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 40, height: 40)
        )
        let mainWindow = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        let picked = SettingsWindowLocator.pick(
            from: [auxiliary, mainWindow], ownedByPIDs: [100]
        )
        XCTAssertEqual(picked, mainWindow)
    }

    func testLocatorIgnoresEveryDecoyAndPicksOnlyTheRealMatch() {
        let wrongPID = TrackedWindowInfo(
            ownerPID: 999, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 900, height: 900)
        )
        let wrongLayer = TrackedWindowInfo(
            ownerPID: 100, layer: 8, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 900, height: 900)
        )
        let offScreen = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: false,
            bounds: CGRect(x: 0, y: 0, width: 900, height: 900)
        )
        let auxiliary = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 30, height: 30)
        )
        let real = TrackedWindowInfo(
            ownerPID: 100, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 50, y: 50, width: 800, height: 600)
        )
        let picked = SettingsWindowLocator.pick(
            from: [wrongPID, wrongLayer, offScreen, auxiliary, real],
            ownedByPIDs: [100]
        )
        XCTAssertEqual(picked, real)
    }

    func testLocatorReturnsNilForAnEmptyWindowList() {
        XCTAssertNil(SettingsWindowLocator.pick(from: [], ownedByPIDs: [100]))
    }

    func testLocatorReturnsNilWhenNoWindowMatchesAnyOwnedPID() {
        let decoy = TrackedWindowInfo(
            ownerPID: 1, layer: 0, isOnScreen: true,
            bounds: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        XCTAssertNil(SettingsWindowLocator.pick(from: [decoy], ownedByPIDs: []))
    }

    // MARK: - PanelFramePlanner

    func testFramePlacesThePanelAtTheTrailingEdgeTopAlignedWithTheYFlip() {
        let bounds = CGRect(x: 100, y: 50, width: 800, height: 600)
        let frame = PanelFramePlanner.frame(
            overSettingsBounds: bounds,
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: CGSize(width: 320, height: 200)
        )
        XCTAssertEqual(frame.origin.x, 100 + 800 + PanelFramePlanner.edgeMargin)
        XCTAssertEqual(frame.origin.y, 1080 - 50 - 200)
        XCTAssertEqual(frame.size, CGSize(width: 320, height: 200))
    }

    func testFrameFollowsAMovedSettingsWindow() {
        let panelSize = CGSize(width: 320, height: 200)
        let before = PanelFramePlanner.frame(
            overSettingsBounds: CGRect(x: 100, y: 50, width: 800, height: 600),
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        let after = PanelFramePlanner.frame(
            overSettingsBounds: CGRect(x: 300, y: 250, width: 800, height: 600),
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        XCTAssertNotEqual(before.origin, after.origin)
        XCTAssertEqual(after.origin.x, 300 + 800 + PanelFramePlanner.edgeMargin)
        XCTAssertEqual(after.origin.y, 1080 - 250 - 200)
    }

    func testFrameFollowsAResizedSettingsWindow() {
        let panelSize = CGSize(width: 320, height: 200)
        let smaller = PanelFramePlanner.frame(
            overSettingsBounds: CGRect(x: 0, y: 0, width: 600, height: 400),
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        let larger = PanelFramePlanner.frame(
            overSettingsBounds: CGRect(x: 0, y: 0, width: 1000, height: 800),
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        XCTAssertEqual(smaller.origin.x, 600 + PanelFramePlanner.edgeMargin)
        XCTAssertEqual(larger.origin.x, 1000 + PanelFramePlanner.edgeMargin)
        XCTAssertNotEqual(smaller.origin.x, larger.origin.x)
        // The panel's own size never changes with the tracked window's size.
        XCTAssertEqual(smaller.size, panelSize)
        XCTAssertEqual(larger.size, panelSize)
    }

    func testFrameClampsToTheScreensTrailingEdgeWhenTheUnclampedXWouldGoOffscreen() {
        // Settings' own trailing edge sits close to the screen's right
        // edge, so `bounds.maxX + edgeMargin` alone would place the
        // panel mostly off-screen.
        let bounds = CGRect(x: 1700, y: 50, width: 800, height: 600)
        let panelSize = CGSize(width: 320, height: 200)
        let frame = PanelFramePlanner.frame(
            overSettingsBounds: bounds,
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        XCTAssertLessThanOrEqual(frame.maxX, 1920 - PanelFramePlanner.edgeMargin)
        XCTAssertGreaterThanOrEqual(frame.origin.x, PanelFramePlanner.edgeMargin)
    }

    func testFrameClampsToTheScreensTopEdgeWhenTheUnclampedYWouldGoOffscreen() {
        // Settings sits near the very top of the screen, so
        // `screenHeight - bounds.minY - panelSize.height` alone would
        // place the panel above the visible top edge.
        let bounds = CGRect(x: 100, y: 5, width: 800, height: 600)
        let panelSize = CGSize(width: 320, height: 200)
        let frame = PanelFramePlanner.frame(
            overSettingsBounds: bounds,
            screenWidth: 1920,
            screenHeight: 1080,
            panelSize: panelSize
        )
        XCTAssertGreaterThanOrEqual(frame.origin.y, PanelFramePlanner.edgeMargin)
        XCTAssertLessThanOrEqual(frame.maxY, 1080 - PanelFramePlanner.edgeMargin)
    }

    func testFallbackFrameCentersThePanelOnScreen() {
        let frame = PanelFramePlanner.fallbackFrame(
            screenWidth: 1440,
            screenHeight: 900,
            panelSize: CGSize(width: 320, height: 200)
        )
        XCTAssertEqual(frame.origin.x, (1440 - 320) / 2)
        XCTAssertEqual(frame.origin.y, (900 - 200) / 2)
        XCTAssertEqual(frame.size, CGSize(width: 320, height: 200))
    }

    // MARK: - TrackingStrategyPicker

    func testStrategyPickerChoosesPollingWhenTheHelperIsNotAXTrusted() {
        XCTAssertEqual(TrackingStrategyPicker.pick(helperIsAXTrusted: false), .cgWindowPolling)
    }

    func testStrategyPickerChoosesAXObserverWhenTheHelperIsAXTrusted() {
        XCTAssertEqual(TrackingStrategyPicker.pick(helperIsAXTrusted: true), .axObserver)
    }

    // MARK: - SystemSettings bundle identifier

    func testSystemSettingsBundleIdentifierIsTheVerifiedConstant() {
        XCTAssertEqual(SystemSettings.bundleIdentifier, "com.apple.systempreferences")
    }
}
