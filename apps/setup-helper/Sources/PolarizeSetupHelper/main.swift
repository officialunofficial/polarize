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
//
// PLZ-9 adds one more reaction, and it is not a permission read either:
// a `SIGUSR1` from the parent process means "my own read says every
// requested permission is now granted," and the helper swaps in a
// success view, then quits — see `SuccessPlan` in `SetupHelperCore` for
// the pure text/timing and `polarize_core::bootstrap::wait_for_grants_or_close`
// for the parent side that sends the signal and still `SIGKILL`s the
// helper afterward regardless (PINV-64 stays exactly as strict as it
// was).
import AppKit
import Dispatch
import SetupHelperCore

/// A borderless, non-activating panel that floats over System
/// Settings without ever taking key/main status or focus away from
/// whatever the user is doing.
final class FloatingHelperPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var panel: FloatingHelperPanel?
    private var tracker: WindowTracker?
    private var successSignalSource: DispatchSourceSignal?

    /// Taller than PLZ-7's plain-text 200pt to fit the drag row
    /// (PLZ-8) beneath the instruction text, when one shows.
    private static let panelSize = NSSize(width: 420, height: 280)

    func applicationDidFinishLaunching(_ notification: Notification) {
        let arguments = Array(CommandLine.arguments.dropFirst())
        let needed = ArgvParser.parse(arguments)
        let bundlePath = ArgvParser.bundlePath(arguments)
        let outcome = LaunchPlanner.run(needed: needed) { urlString in
            guard let url = URL(string: urlString) else { return false }
            return NSWorkspace.shared.open(url)
        }
        let dragPayload = DragSourcePlanner.payload(outcome: outcome, bundlePath: bundlePath)

        let panel = Self.makePanel()
        panel.contentView = Self.makeMessageView(
            needed: needed,
            outcome: outcome,
            dragPayload: dragPayload,
            bundlePath: bundlePath,
            onDragStateChange: { [weak self] isDragging in
                self?.setDraggingPassthrough(isDragging)
            }
        )
        panel.orderFront(nil)
        self.panel = panel

        let tracker = WindowTracker(panel: panel, panelSize: Self.panelSize)
        tracker.start()
        self.tracker = tracker

        installSuccessSignalHandler()
    }

    /// Switches the panel into a drag-friendly mode: mouse-transparent
    /// and ordered behind other windows, so it can never intercept the
    /// drop meant for System Settings underneath it, and pauses window
    /// tracking so it can't reposition the panel out from under an
    /// in-progress drag. Modeled on `jaywcjlove/PermissionFlow`'s
    /// `FloatingDropPanel.setDraggingPassthrough`.
    @MainActor
    private func setDraggingPassthrough(_ isDragging: Bool) {
        panel?.ignoresMouseEvents = isDragging
        panel?.alphaValue = isDragging ? 0.72 : 1.0
        if isDragging {
            panel?.orderBack(nil)
            tracker?.suspend()
        } else {
            panel?.orderFront(nil)
            tracker?.resume()
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    /// Reacts to the parent's `SIGUSR1` (PLZ-9): swaps in the success
    /// view, then quits after `SuccessPlan.quitDelaySeconds`. Calls no
    /// permission API — it only reacts to a signal the parent process
    /// sent, after the parent's own non-prompting re-read already
    /// decided every requested permission is granted (PINV-56, PINV-58).
    ///
    /// `signal(SIGUSR1, SIG_IGN)` first, so the raw POSIX signal never
    /// terminates the process before `DispatchSource` gets a chance to
    /// dispatch its handler onto the main queue — this is
    /// `DispatchSource.makeSignalSource`'s own documented setup
    /// requirement. The dispatch source itself is retained on `self`;
    /// an unretained source would be deallocated (and cancelled)
    /// immediately after this method returns.
    private func installSuccessSignalHandler() {
        signal(SIGUSR1, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        source.setEventHandler { [weak self] in
            // Safe: `.main` queue dispatches serially on the main
            // thread, exactly like `WindowTracker.swift`'s own
            // `Timer` callback above it in this same codebase.
            MainActor.assumeIsolated {
                self?.handleAllGrantedSignal()
            }
        }
        source.resume()
        successSignalSource = source
    }

    @MainActor
    private func handleAllGrantedSignal() {
        tracker?.stop()
        panel?.contentView = Self.makeSuccessView()
        DispatchQueue.main.asyncAfter(deadline: .now() + SuccessPlan.quitDelaySeconds) {
            exit(0)
        }
    }

    /// Builds the success view: the same translucent rounded card the
    /// instruction view uses, holding only `SuccessPlan.message` — a
    /// fact the parent process reported, never anything the helper
    /// checked for itself.
    @MainActor
    private static func makeSuccessView() -> NSView {
        let backing = NSVisualEffectView()
        backing.translatesAutoresizingMaskIntoConstraints = false
        backing.material = .hudWindow
        backing.state = .active
        backing.wantsLayer = true
        backing.layer?.cornerRadius = 14
        backing.layer?.masksToBounds = true

        let text = NSTextField(wrappingLabelWithString: SuccessPlan.message)
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
    /// transparent window, even on the fallback path. Adds the drag
    /// row (PLZ-8) beneath the text only when `dragPayload` is
    /// non-nil — `DragSourcePlanner` already restricts that to an
    /// Accessibility or Screen Recording launch with a known bundle
    /// path (PINV-59, PINV-60).
    @MainActor
    private static func makeMessageView(
        needed: [NeededPermission],
        outcome: LaunchOutcome,
        dragPayload: DragPayload?,
        bundlePath: String?,
        onDragStateChange: @escaping (Bool) -> Void
    ) -> NSView {
        let backing = NSVisualEffectView()
        backing.translatesAutoresizingMaskIntoConstraints = false
        backing.material = .hudWindow
        backing.state = .active
        backing.wantsLayer = true
        backing.layer?.cornerRadius = 14
        backing.layer?.masksToBounds = true

        let text = NSTextField(
            wrappingLabelWithString: message(for: needed, outcome: outcome, hasDragPayload: dragPayload != nil)
        )
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

        if let dragPayload, let bundlePath {
            let dragView = AppIconDragView(payload: dragPayload, bundlePath: bundlePath)
            dragView.onDragStateChange = onDragStateChange
            dragView.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(dragView)
            NSLayoutConstraint.activate([
                dragView.topAnchor.constraint(equalTo: text.bottomAnchor, constant: 16),
                // Explicit leading/trailing (not just centerX) so
                // Auto Layout can actually resolve dragView's width.
                // Without these, its width is ambiguous and resolves
                // near zero — the icon/caption still draw where their
                // own constraints put them relative to dragView's
                // origin, since NSView doesn't clip to bounds by
                // default, but dragView's real hit-testable frame ends
                // up far smaller than what's visually drawn. Confirmed
                // live: this is why the icon rendered but was
                // completely unclickable, even with the `hitTest`
                // override on `AppIconDragView` itself.
                dragView.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 20),
                dragView.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -20),
                dragView.bottomAnchor.constraint(lessThanOrEqualTo: container.bottomAnchor, constant: -20),
            ])
        }
        return container
    }

    private static func message(
        for needed: [NeededPermission],
        outcome: LaunchOutcome,
        hasDragPayload: Bool
    ) -> String {
        let names = needed.map(name(for:)).joined(separator: ", ")
        let needLine = names.isEmpty ? "Polarize needs no further permission." : "Polarize still needs: \(names)."
        let dragHint = hasDragPayload ? " Or drag this icon into the list." : ""

        switch outcome {
        case .openedPane(let permission, _):
            return "\(needLine)\n\(enableInstruction(for: permission)).\(dragHint)"
        case .fellBackToInstructions(let permission, _):
            return
                "\(needLine)\nSystem Settings could not open the exact pane. "
                + "Open System Settings > Privacy & Security, find the matching entry, and \(enableInstruction(for: permission, capitalized: false))."
                + dragHint
        case .nothingToOpen:
            // PLZ-10: Automation always maps to a pane now, so this
            // path only ever fires for an empty set or a set naming
            // only `.unknown` permissions — never for Automation.
            return "\(needLine)\nOpen System Settings > Privacy & Security and enable Polarize."
        }
    }

    /// The pane-opened instruction sentence, worded per permission.
    /// Automation's checkbox lives under its target app's own row, so
    /// naming that target (rather than the generic "enable Polarize")
    /// is the difference between a usable instruction and a dead end.
    private static func enableInstruction(for permission: NeededPermission, capitalized: Bool = true) -> String {
        switch permission {
        case .automation(let target):
            let verb = capitalized ? "Allow" : "allow"
            return "\(verb) Polarize to control \(target) in the Automation list"
        case .accessibility, .screenRecording, .unknown:
            let verb = capitalized ? "Open" : "open"
            return "\(verb) System Settings > Privacy & Security and enable Polarize"
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
