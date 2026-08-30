import XCTest

@testable import SetupHelperCore

final class ChecklistItemTests: XCTestCase {
    func testAccessibilityItemPreMacOS27UsesTheOldName() {
        let item = PermissionChecklist.item(for: .accessibility, osMajorVersion: 26)
        XCTAssertEqual(item.permission, .accessibility)
        XCTAssertEqual(item.title, "Accessibility")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.accessibility")
        XCTAssertEqual(item.symbolName, "accessibility")
        XCTAssertFalse(item.detail.isEmpty)
    }

    func testAccessibilityItemMacOS27AndLaterUsesTheRenamedPane() {
        let item = PermissionChecklist.item(for: .accessibility, osMajorVersion: 27)
        XCTAssertEqual(item.title, "Device Control and Data Access")

        let later = PermissionChecklist.item(for: .accessibility, osMajorVersion: 28)
        XCTAssertEqual(later.title, "Device Control and Data Access")
    }

    func testScreenRecordingItemPreMacOS15UsesTheOldName() {
        let item = PermissionChecklist.item(for: .screenRecording, osMajorVersion: 14)
        XCTAssertEqual(item.permission, .screenRecording)
        XCTAssertEqual(item.title, "Screen Recording")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.screen-recording")
        XCTAssertEqual(item.symbolName, "record.circle.fill")
    }

    func testScreenRecordingItemMacOS15AndLaterUsesTheRenamedPane() {
        let item = PermissionChecklist.item(for: .screenRecording, osMajorVersion: 15)
        XCTAssertEqual(item.title, "Screen & System Audio Recording")

        let later = PermissionChecklist.item(for: .screenRecording, osMajorVersion: 26)
        XCTAssertEqual(later.title, "Screen & System Audio Recording")
    }

    func testAutomationItemNamesItsTarget() {
        let item = PermissionChecklist.item(for: .automation(target: "Finder"), osMajorVersion: 26)
        XCTAssertEqual(item.title, "Automation (Finder)")
        XCTAssertEqual(item.graphicIconUTI, "com.apple.graphic-icon.automation")
        XCTAssertEqual(item.symbolName, "gearshape.2.fill")
        XCTAssertTrue(item.detail.contains("Finder"))
    }

    func testUnknownItemUsesTheRawValueAsATitle() {
        let item = PermissionChecklist.item(for: .unknown("mystery"), osMajorVersion: 26)
        XCTAssertEqual(item.title, "mystery")
        XCTAssertNil(item.graphicIconUTI)
    }

    func testItemsPreservesOrderAndCount() {
        let needed: [NeededPermission] = [.screenRecording, .accessibility, .automation(target: "Mail")]
        let items = PermissionChecklist.items(for: needed, osMajorVersion: 26)
        XCTAssertEqual(items.map(\.permission), needed)
    }

    func testItemsIsEmptyForAnEmptyList() {
        XCTAssertTrue(PermissionChecklist.items(for: [], osMajorVersion: 26).isEmpty)
    }
}
