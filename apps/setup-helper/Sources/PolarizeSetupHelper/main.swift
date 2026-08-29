// `PolarizeSetupHelper` reads the `--needs` argv `polarize
// --request-permissions` builds (`polarize_core::bootstrap::helper_args`)
// and opens the matching System Settings pane. All parsing, pane
// mapping, and fallback selection live in `SetupHelperCore`, a plain
// module with no AppKit import — this file only wires that pure logic
// to `NSWorkspace.shared.open` and renders the window text. It calls
// no TCC-touching API of any kind — no `AXIsProcessTrusted`, no
// `CGPreflightScreenCaptureAccess`, no Apple Event send.
// `NSWorkspace.shared.open` on a settings URL is not such an API: it
// opens an app, it does not request a grant. See PINV-58 in
// `docs/INVARIANTS.md` for the rule this file must keep holding.
import AppKit
import SetupHelperCore

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let needed = ArgvParser.parse(Array(CommandLine.arguments.dropFirst()))
        let outcome = LaunchPlanner.run(needed: needed) { urlString in
            guard let url = URL(string: urlString) else { return false }
            return NSWorkspace.shared.open(url)
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 300),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Polarize Setup Helper"
        window.contentView = Self.makeMessageView(needed: needed, outcome: outcome)
        window.center()
        window.makeKeyAndOrderFront(nil)
        self.window = window
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    /// Builds the window's text so it is never blank (PINV-63), even
    /// on the fallback path.
    @MainActor
    private static func makeMessageView(needed: [NeededPermission], outcome: LaunchOutcome) -> NSView {
        let text = NSTextField(wrappingLabelWithString: message(for: needed, outcome: outcome))
        text.translatesAutoresizingMaskIntoConstraints = false
        text.font = .systemFont(ofSize: 13)

        let container = NSView()
        container.addSubview(text)
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 20),
            text.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -20),
            text.topAnchor.constraint(equalTo: container.topAnchor, constant: 20),
        ])
        return container
    }

    private static func message(for needed: [NeededPermission], outcome: LaunchOutcome) -> String {
        let names = needed.map(name(for:)).joined(separator: ", ")
        let needLine = names.isEmpty ? "Polarize needs no further permission." : "Polarize still needs: \(names)."

        switch outcome {
        case .openedPane:
            return "\(needLine)\nOpen System Settings > Privacy & Security and enable Polarize."
        case .fellBackToInstructions:
            return
                "\(needLine)\nSystem Settings could not open the exact pane. "
                + "Open System Settings > Privacy & Security, find the matching entry, and enable Polarize."
        case .nothingToOpen:
            return "\(needLine)\nGrant Automation access to Polarize when macOS prompts for it."
        }
    }

    private static func name(for permission: NeededPermission) -> String {
        switch permission {
        case .accessibility:
            return "Accessibility"
        case .screenRecording:
            return "Screen Recording"
        case .automation(let target):
            return "Automation (\(target))"
        case .unknown(let value):
            return value
        }
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.activate(ignoringOtherApps: true)
app.run()
