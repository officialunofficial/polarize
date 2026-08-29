// `PolarizeSetupHelper` is a skeleton today. It shows one plain
// window and nothing else. It calls no TCC-touching API of any kind —
// no `AXIsProcessTrusted`, no `CGPreflightScreenCaptureAccess`, no
// Apple Event send. PLZ-3's guided-permission flow builds on top of
// this skeleton later. See PINV-58 in `docs/INVARIANTS.md` for the
// rule this file must keep holding once that flow lands: the helper
// never requests a TCC grant of its own.
import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 300),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Polarize Setup Helper"
        window.center()
        window.makeKeyAndOrderFront(nil)
        self.window = window
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.activate(ignoringOtherApps: true)
app.run()
