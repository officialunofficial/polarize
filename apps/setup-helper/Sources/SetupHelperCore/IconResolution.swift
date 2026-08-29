// Pure path arithmetic for loading a bundle's own icon file directly,
// bypassing `NSWorkspace.shared.icon(forFile:)`. Confirmed live
// (2026-08-29, against this repo's own freshly built `dist/Polarize.app`
// on a real Mac): `NSWorkspace.shared.icon(forFile:)` returned a
// generic 32x32 icon with no real artwork for a bundle LaunchServices
// had not yet indexed — a fresh, non-installed build, self-signed with
// a dev identity, is exactly that case. Loading the same bundle's
// `CFBundleIconFile` straight from `Contents/Resources/` returned the
// real 512x512, 10-representation icon. No AppKit import — this file
// only builds the file path; `PolarizeSetupHelper` does the actual
// `NSImage(contentsOfFile:)` load.
import Foundation

public enum BundleIconResolver {
    /// Joins a bundle's path with its `CFBundleIconFile` value into the
    /// file `NSImage(contentsOfFile:)` should load, appending `.icns`
    /// when the Info.plist value omits the extension (the common case
    /// — see `apps/polarize/bundle/Info.plist.in`'s own
    /// `CFBundleIconFile`).
    public static func iconPath(bundlePath: String, iconFileName: String) -> String {
        let fileName = iconFileName.hasSuffix(".icns") ? iconFileName : iconFileName + ".icns"
        return bundlePath + "/Contents/Resources/" + fileName
    }
}
