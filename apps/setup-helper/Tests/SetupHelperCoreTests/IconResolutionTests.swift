import XCTest

@testable import SetupHelperCore

final class IconResolutionTests: XCTestCase {
    func testAppendsIcnsExtensionWhenMissing() {
        XCTAssertEqual(
            BundleIconResolver.iconPath(bundlePath: "/Applications/Polarize.app", iconFileName: "Polarize"),
            "/Applications/Polarize.app/Contents/Resources/Polarize.icns"
        )
    }

    func testKeepsAnExplicitIcnsExtensionAsIs() {
        XCTAssertEqual(
            BundleIconResolver.iconPath(bundlePath: "/Applications/Polarize.app", iconFileName: "Polarize.icns"),
            "/Applications/Polarize.app/Contents/Resources/Polarize.icns"
        )
    }

    func testHandlesABundlePathWithSpaces() {
        XCTAssertEqual(
            BundleIconResolver.iconPath(bundlePath: "/Users/me/dev build/Polarize.app", iconFileName: "Polarize"),
            "/Users/me/dev build/Polarize.app/Contents/Resources/Polarize.icns"
        )
    }
}
