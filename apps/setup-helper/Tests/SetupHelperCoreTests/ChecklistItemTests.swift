import XCTest

@testable import SetupHelperCore

final class ChecklistItemTests: XCTestCase {
    func testAccessibilityItem() {
        let item = PermissionChecklist.item(for: .accessibility)
        XCTAssertEqual(item.permission, .accessibility)
        XCTAssertEqual(item.title, "Accessibility")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.accessibility")
        XCTAssertEqual(item.symbolName, "accessibility")
        XCTAssertFalse(item.detail.isEmpty)
    }

    func testScreenRecordingItem() {
        let item = PermissionChecklist.item(for: .screenRecording)
        XCTAssertEqual(item.permission, .screenRecording)
        XCTAssertEqual(item.title, "Screen Recording")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.screen-recording")
        XCTAssertEqual(item.symbolName, "record.circle.fill")
    }

    func testAutomationItemNamesItsTarget() {
        let item = PermissionChecklist.item(for: .automation(target: "Finder"))
        XCTAssertEqual(item.title, "Automation (Finder)")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.automation")
        XCTAssertEqual(item.symbolName, "gearshape.2.fill")
        XCTAssertTrue(item.detail.contains("Finder"))
    }

    func testUnknownItemUsesTheRawValueAsATitle() {
        let item = PermissionChecklist.item(for: .unknown("mystery"))
        XCTAssertEqual(item.title, "mystery")
        XCTAssertNil(item.graphicIconUTI)
    }

    func testItemsPreservesOrderAndCount() {
        let needed: [NeededPermission] = [.screenRecording, .accessibility, .automation(target: "Mail")]
        let items = PermissionChecklist.items(for: needed)
        XCTAssertEqual(items.map(\.permission), needed)
    }

    func testItemsIsEmptyForAnEmptyList() {
        XCTAssertTrue(PermissionChecklist.items(for: []).isEmpty)
    }
}
