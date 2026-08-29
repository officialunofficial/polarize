// `PolarizeSetupHelper` reads the `--needs` argv `polarize
// --request-permissions` builds (`polarize_core::bootstrap::helper_args`)
// and opens the matching System Settings pane. All parsing, pane
// mapping, and fallback selection live in `SetupHelperCore`, a plain
// module with no AppKit import — this file only wires that pure logic
// to `NSWorkspace.shared.open` and renders the window text.
//
// This file calls exactly one TCC-adjacent API, and it is
// non-prompting: `WindowTracker` reads the helper's own
// `AXIsProcessTrusted()` once, at launch, purely to pick between two
// window-tracking mechanisms (PINV-62). That read never prompts, never
// answers for Polarize's own bundle, and is never shown to the user as
// Polarize's grant state — PINV-56 and PINV-58 both stay satisfied.
// `NSWorkspace.shared.open` on a settings URL is not a TCC API either:
// it opens an app, it does not request a grant. See PINV-58 in
// `docs/INVARIANTS.md` for the rule this file must keep holding.
import AppKit
import SetupHelperCore

/// A borderless, non-activating panel that floats over System
/// Settings without ever taking key/main status or focus away from
/// whatever the user is doing.
final class FloatingHelperPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var panel: FloatingHelperPanel?
    private var tracker: WindowTracker?

    private static let panelSize = NSSize(width: 420, height: 200)

    func applicationDidFinishLaunching(_ notification: Notification) {
        let needed = ArgvParser.parse(Array(CommandLine.arguments.dropFirst()))
        let outcome = LaunchPlanner.run(needed: needed) { urlString in
            guard let url = URL(string: urlString) else { return false }
            return NSWorkspace.shared.open(url)
        }

        let panel = Self.makePanel()
        panel.contentView = Self.makeMessageView(needed: needed, outcome: outcome)
        panel.orderFront(nil)
        self.panel = panel

        let tracker = WindowTracker(panel: panel, panelSize: Self.panelSize)
        tracker.start()
        self.tracker = tracker
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    /// Builds the floating panel itself: borderless, never key/main,
    /// floating above ordinary windows, visible on every Space
    /// (including a full-screen one), and never opaque — the content
    /// view below draws its own translucent card so the message text
    /// stays legible against whatever sits behind the panel.
    @MainActor
    private static func makePanel() -> FloatingHelperPanel {
        let panel = FloatingHelperPanel(
            contentRect: NSRect(origin: .zero, size: panelSize),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = .floating
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isReleasedWhenClosed = false
        return panel
    }

    /// Builds the panel's text on a translucent rounded backing, so it
    /// is never blank (PINV-63) and stays readable on a fully
    /// transparent window, even on the fallback path.
    @MainActor
    private static func makeMessageView(needed: [NeededPermission], outcome: LaunchOutcome) -> NSView {
        let backing = NSVisualEffectView()
        backing.translatesAutoresizingMaskIntoConstraints = false
        backing.material = .hudWindow
        backing.state = .active
        backing.wantsLayer = true
        backing.layer?.cornerRadius = 14
        backing.layer?.masksToBounds = true

        let text = NSTextField(wrappingLabelWithString: message(for: needed, outcome: outcome))
        text.translatesAutoresizingMaskIntoConstraints = false
        text.font = .systemFont(ofSize: 13)

        let container = NSView()
        container.addSubview(backing)
        container.addSubview(text)
        NSLayoutConstraint.activate([
            backing.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            backing.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            backing.topAnchor.constraint(equalTo: container.topAnchor),
            backing.bottomAnchor.constraint(equalTo: container.bottomAnchor),
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
// `.accessory`, with no `activate(ignoringOtherApps:)` call: the
// helper never gets a Dock icon, never becomes the frontmost app, and
// never steals focus or key/main status from whatever the user is
// doing — see the header comment and PINV-62.
app.setActivationPolicy(.accessory)
app.run()
