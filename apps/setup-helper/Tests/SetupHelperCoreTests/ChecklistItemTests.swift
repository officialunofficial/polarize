import XCTest

@testable import SetupHelperCore

final class ChecklistItemTests: XCTestCase {
    func testAccessibilityItem() {
        let item = PermissionChecklist.item(for: .accessibility)
        XCTAssertEqual(item.permission, .accessibility)
        XCTAssertEqual(item.title, "Accessibility")
        XCTAssertEqual(item.symbolName, "accessibility")
        XCTAssertFalse(item.detail.isEmpty)
    }

    func testScreenRecordingItem() {
        let item = PermissionChecklist.item(for: .screenRecording)
        XCTAssertEqual(item.permission, .screenRecording)
        XCTAssertEqual(item.title, "Screen Recording")
        XCTAssertEqual(item.symbolName, "camera.viewfinder")
    }

    func testAutomationItemNamesItsTarget() {
        let item = PermissionChecklist.item(for: .automation(target: "Finder"))
        XCTAssertEqual(item.title, "Automation (Finder)")
        XCTAssertTrue(item.detail.contains("Finder"))
    }

    func testUnknownItemUsesTheRawValueAsATitle() {
        let item = PermissionChecklist.item(for: .unknown("mystery"))
        XCTAssertEqual(item.title, "mystery")
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
