// Shared by `DragSourceView.swift` and `main.swift` — both needed the
// exact same bundle-icon-loading logic (previously duplicated in
// both files verbatim; a code review flagged the duplication as real,
// not merely similar-shaped code).
import AppKit
import SetupHelperCore

enum HelperIconLoader {
    /// Loads a bundle's own icon straight from `Contents/Resources/`
    /// instead of `NSWorkspace.shared.icon(forFile:)`. Confirmed live:
    /// `NSWorkspace` returns a generic, low-resolution icon for a
    /// freshly built, non-installed bundle LaunchServices has not yet
    /// indexed — exactly the case for a bundle the helper was just
    /// handed via `--for-bundle`. Falls back to `NSWorkspace` only if
    /// the direct load fails.
    static func icon(forBundleAt bundlePath: String) -> NSImage {
        if let bundle = Bundle(path: bundlePath),
            let iconFileName = bundle.infoDictionary?["CFBundleIconFile"] as? String
        {
            let iconPath = BundleIconResolver.iconPath(bundlePath: bundlePath, iconFileName: iconFileName)
            if let direct = NSImage(contentsOfFile: iconPath) {
                return direct
            }
        }
        return NSWorkspace.shared.icon(forFile: bundlePath)
    }
}
