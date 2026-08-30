// `PolarizeSetupHelper` reads the `--needs` argv `polarize
// --request-permissions` builds (`polarize_core::bootstrap::helper_args`)
// and shows two screens in turn, modeled on OpenAI Codex's own
// "Enable Codex Computer Use" onboarding: first `ChecklistWindow`, a
// real titled/closable window listing every still-needed permission
// with an "Allow" button; then, per permission the user taps Allow
// on, the non-activating `FloatingHelperPanel` that opens the matching
// System Settings pane and floats over it. All parsing, pane mapping,
// and fallback selection live in `SetupHelperCore`, a plain module
// with no AppKit import — this file only wires that pure logic to
// `NSWorkspace.shared.open` and renders the two windows.
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
// requested permission is now granted," and the helper shows a
// success panel, then quits — see `SuccessPlan` in `SetupHelperCore`
// for the pure text/timing and
// `polarize_core::bootstrap::wait_for_grants_or_close` for the parent
// side that sends the signal and still `SIGKILL`s the helper
// afterward regardless (PINV-64 stays exactly as strict as it was).
import AppKit
import Dispatch
import SetupHelperCore

/// A borderless, non-activating panel that floats over System
/// Settings without ever taking key/main status or focus away from
/// whatever the user is doing. Used only for the per-permission guide
/// screen and the final success screen — never for `ChecklistWindow`,
/// which has nothing to coexist with on screen and behaves like an
/// ordinary window instead.
final class FloatingHelperPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var checklistWindow: ChecklistWindow?
    private var panel: FloatingHelperPanel?
    private var tracker: WindowTracker?
    private var successSignalSource: DispatchSourceSignal?
    private var bundlePath: String?

    /// The full, fixed set of permissions this launch was asked to
    /// help with, in argv order — set once, at launch, never mutated.
    /// Lets the guide screen show "N of M" and step through the list
    /// with Next/Previous, without needing to know whether any of them
    /// has actually been granted yet (only the parent process can know
    /// that — see `handleAllGrantedSignal`'s own doc comment).
    private var neededPermissions: [NeededPermission] = []

    /// Taller than PLZ-7's plain-text 200pt to fit the drag row
    /// (PLZ-8) and the back button (this follow-up) beneath the
    /// instruction text, when they show.
    private static let panelSize = NSSize(width: 420, height: 300)

    func applicationDidFinishLaunching(_ notification: Notification) {
        let arguments = Array(CommandLine.arguments.dropFirst())
        let needed = ArgvParser.parse(arguments)
        bundlePath = ArgvParser.bundlePath(arguments)
        installSuccessSignalHandler()

        guard !needed.isEmpty else {
            // Nothing named — an unknown-only or empty argv. Falls
            // back to the same guide panel a real permission would
            // get, worded for the empty case, so the window is never
            // blank (PINV-63's spirit) even here.
            showGuidePanel(outcome: .nothingToOpen, index: nil)
            return
        }

        neededPermissions = needed
        let osMajorVersion = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        let items = PermissionChecklist.items(for: needed, osMajorVersion: osMajorVersion)
        let appIcon = bundlePath.map(HelperIconLoader.icon(forBundleAt:))
        let checklist = ChecklistWindow(items: items, appIcon: appIcon)
        checklist.onAllowTapped = { [weak self] permission in
            guard let index = self?.neededPermissions.firstIndex(of: permission) else { return }
            self?.beginGuide(at: index)
        }
        checklist.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        checklistWindow = checklist
    }

    /// Always `false`. Confirmed live as a real bug: `FloatingHelperPanel`
    /// (a borderless, non-activating `NSPanel`) does not count toward
    /// AppKit's own "last window closed" bookkeeping the way an
    /// ordinary `NSWindow` does. Ordering `ChecklistWindow` out to show
    /// the guide panel — a deliberate, successful transition, not a
    /// user-initiated close — was being read as "the last window just
    /// closed," terminating the whole app moments after the guide
    /// panel had already appeared. This process no longer needs that
    /// heuristic at all: it always terminates itself explicitly, either
    /// via the success signal's `exit(0)` (PLZ-9) or the parent's own
    /// `SIGKILL` after its deadline (PINV-61, PINV-64) — never by
    /// inferring intent from which windows happen to be visible.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    // MARK: - Screen transitions

    /// Starts the guide screen for `neededPermissions[index]`: opens
    /// its System Settings pane and shows the floating panel over it,
    /// with a drag target for Accessibility/Screen Recording, a "N of
    /// M" progress line, and Previous/Next buttons that step through
    /// the rest of the list directly — added after a live report that
    /// the only way back to the list (Back, to the checklist) left no
    /// clue what to do next for a second or third permission.
    private func beginGuide(at index: Int) {
        checklistWindow?.orderOut(nil)
        let permission = neededPermissions[index]
        let outcome = LaunchPlanner.run(needed: [permission]) { urlString in
            guard let url = URL(string: urlString) else { return false }
            return NSWorkspace.shared.open(url)
        }
        showGuidePanel(outcome: outcome, index: index)
    }

    /// Returns from the guide screen to the checklist, e.g. after the
    /// user taps the back button — lets them jump to any still-needed
    /// permission directly, rather than only stepping through them in
    /// order (the parent only signals once *all* are granted — see
    /// `handleAllGrantedSignal` — so this is the only way to revisit
    /// the list mid-flow).
    private func returnToChecklist() {
        tracker?.stop()
        tracker = nil
        panel?.orderOut(nil)
        panel = nil
        checklistWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func showGuidePanel(outcome: LaunchOutcome, index: Int?) {
        // Explicitly tear down any existing guide panel/tracker before
        // creating new ones. Confirmed live as a real bug: stepping
        // through Previous/Next called this repeatedly, each time
        // just overwriting `self.panel`/`self.tracker` and trusting
        // ARC to deallocate (and thereby close) the old ones — which
        // did not reliably happen, leaving old panels stacked on
        // screen and their still-running timers pointlessly polling.
        // It also meant `handleAllGrantedSignal` only ever closed the
        // *current* panel, leaving any leaked older one open forever
        // even once every permission was granted.
        tracker?.stop()
        tracker = nil
        panel?.orderOut(nil)
        panel = nil

        let dragPayload = DragSourcePlanner.payload(outcome: outcome, bundlePath: bundlePath)
        let panel = Self.makePanel()
        var progress: GuideProgress?
        var onBackTapped: (() -> Void)?
        var onPreviousTapped: (() -> Void)?
        var onNextTapped: (() -> Void)?
        if let index {
            progress = GuideProgress(index: index, count: neededPermissions.count)
            onBackTapped = { [weak self] in self?.returnToChecklist() }
            if index > 0 {
                onPreviousTapped = { [weak self] in self?.beginGuide(at: index - 1) }
            }
            if index < neededPermissions.count - 1 {
                onNextTapped = { [weak self] in self?.beginGuide(at: index + 1) }
            }
        }
        panel.contentView = Self.makeGuideView(
            outcome: outcome,
            dragPayload: dragPayload,
            bundlePath: bundlePath,
            progress: progress,
            onDragStateChange: { [weak self] isDragging in
                self?.setDraggingPassthrough(isDragging)
            },
            onBackTapped: onBackTapped,
            onPreviousTapped: onPreviousTapped,
            onNextTapped: onNextTapped
        )
        panel.orderFront(nil)
        self.panel = panel

        let tracker = WindowTracker(panel: panel, panelSize: Self.panelSize)
        tracker.start()
        self.tracker = tracker
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

    // MARK: - PLZ-9's success signal

    /// Reacts to the parent's `SIGUSR1` (PLZ-9): closes whichever
    /// screen is showing and shows a small success panel, then quits
    /// after `SuccessPlan.quitDelaySeconds`. Calls no permission API —
    /// it only reacts to a signal the parent process sent, after the
    /// parent's own non-prompting re-read already decided every
    /// requested permission is granted (PINV-56, PINV-58).
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
        tracker = nil
        checklistWindow?.orderOut(nil)
        checklistWindow = nil

        let successPanel = panel ?? Self.makePanel()
        successPanel.contentView = Self.makeSuccessView()
        successPanel.center()
        successPanel.orderFront(nil)
        panel = successPanel

        DispatchQueue.main.asyncAfter(deadline: .now() + SuccessPlan.quitDelaySeconds) {
            exit(0)
        }
    }

    /// Builds the success view. It is the same rounded material card
    /// the guide view uses. It holds only `SuccessPlan.message`, a
    /// fact the parent process reported. The helper never checks this
    /// for itself.
    @MainActor
    private static func makeSuccessView() -> NSView {
        let text = NSTextField(wrappingLabelWithString: SuccessPlan.message)
        text.translatesAutoresizingMaskIntoConstraints = false
        text.font = .preferredFont(forTextStyle: .body, options: [:])

        let content = NSView()
        content.addSubview(text)
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            text.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            text.topAnchor.constraint(equalTo: content.topAnchor, constant: 20),
            text.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -20),
        ])
        return wrapInMaterial(content: content, cornerRadius: 14)
    }

    /// Backs `content` with the panel's rounded translucent card. It
    /// calls the shared `MaterialBackground.wrap`, also used by
    /// `ChecklistWindow`. See that file for the Liquid Glass
    /// availability branch itself.
    @MainActor
    private static func wrapInMaterial(content: NSView, cornerRadius: CGFloat) -> NSView {
        MaterialBackground.wrap(content: content, cornerRadius: cornerRadius, material: .hudWindow)
    }

    // MARK: - The floating guide panel

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

    /// Builds the guide panel's text and, when relevant, its drag row,
    /// back button, progress line, and Previous/Next navigation, on a
    /// translucent rounded backing — never blank (PINV-63), and
    /// readable on the fully transparent panel even on the fallback
    /// path. `progress` is `nil` only for the argv-empty/unknown-only
    /// launch, which has no checklist screen or sibling permissions to
    /// navigate between.
    @MainActor
    private static func makeGuideView(
        outcome: LaunchOutcome,
        dragPayload: DragPayload?,
        bundlePath: String?,
        progress: GuideProgress?,
        onDragStateChange: @escaping (Bool) -> Void,
        onBackTapped: (() -> Void)?,
        onPreviousTapped: (() -> Void)?,
        onNextTapped: (() -> Void)?
    ) -> NSView {
        let content = NSView()

        var topAnchor = content.topAnchor
        var topConstant: CGFloat = 20
        if let onBackTapped {
            let back = BackButton(onTapped: onBackTapped)
            back.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(back)
            NSLayoutConstraint.activate([
                back.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 12),
                back.topAnchor.constraint(equalTo: content.topAnchor, constant: 12),
            ])

            if let progress {
                let progressLabel = NSTextField(labelWithString: progress.text)
                progressLabel.translatesAutoresizingMaskIntoConstraints = false
                progressLabel.font = .preferredFont(forTextStyle: .caption1, options: [:])
                progressLabel.textColor = .secondaryLabelColor
                content.addSubview(progressLabel)
                NSLayoutConstraint.activate([
                    progressLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -12),
                    progressLabel.centerYAnchor.constraint(equalTo: back.centerYAnchor),
                ])
            }

            topAnchor = back.bottomAnchor
            topConstant = 8
        }

        let text = NSTextField(wrappingLabelWithString: message(for: outcome, hasDragPayload: dragPayload != nil))
        text.translatesAutoresizingMaskIntoConstraints = false
        text.font = .preferredFont(forTextStyle: .body, options: [:])
        content.addSubview(text)
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            text.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            text.topAnchor.constraint(equalTo: topAnchor, constant: topConstant),
        ])

        var lastAnchor = text.bottomAnchor
        var lastConstant: CGFloat = 16
        var dragView: AppIconDragView?

        if let dragPayload, let bundlePath {
            let view = AppIconDragView(payload: dragPayload, bundlePath: bundlePath)
            view.onDragStateChange = onDragStateChange
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
            NSLayoutConstraint.activate([
                view.topAnchor.constraint(equalTo: lastAnchor, constant: lastConstant),
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
                view.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
                view.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            ])
            lastAnchor = view.bottomAnchor
            lastConstant = 12
            dragView = view
        }

        if onPreviousTapped != nil || onNextTapped != nil {
            let navRow = NSStackView()
            navRow.orientation = .horizontal
            navRow.distribution = .equalSpacing
            navRow.translatesAutoresizingMaskIntoConstraints = false

            if let onPreviousTapped {
                navRow.addArrangedSubview(
                    NavButton(title: "Previous", symbolName: "chevron.left", leading: true, onTapped: onPreviousTapped)
                )
            }
            if let onNextTapped {
                let nextButton = NavButton(
                    title: "Next", symbolName: "chevron.right", leading: false, onTapped: onNextTapped
                )
                // Hidden until the user has actually attempted the
                // current step's drag. This was added after a live
                // report: Next let the user skip ahead before even
                // trying the drag. Automation's guide has no drag view.
                // There is nothing to gate on there, so Next stays
                // visible from the start in that case.
                if dragView != nil {
                    nextButton.isHidden = true
                    dragView?.onDragCompleted = { [weak nextButton] in
                        nextButton?.isHidden = false
                    }
                }
                navRow.addArrangedSubview(nextButton)
            }

            content.addSubview(navRow)
            NSLayoutConstraint.activate([
                navRow.topAnchor.constraint(equalTo: lastAnchor, constant: lastConstant),
                navRow.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
                navRow.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
                navRow.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -16),
            ])
        } else {
            lastAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -20).isActive = true
        }

        return wrapInMaterial(content: content, cornerRadius: 14)
    }

    private static func message(for outcome: LaunchOutcome, hasDragPayload: Bool) -> String {
        let dragHint = hasDragPayload ? " Or drag this icon into the list." : ""
        switch outcome {
        case .openedPane(let permission, _):
            return "\(enableInstruction(for: permission)).\(dragHint)"
        case .fellBackToInstructions(let permission, _):
            return
                "System Settings could not open the exact pane. "
                + "Open System Settings > Privacy & Security, find the matching entry, and \(enableInstruction(for: permission, capitalized: false))."
                + dragHint
        case .nothingToOpen:
            return "Open System Settings > Privacy & Security and enable Polarize."
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
}

/// A small "Back" affordance in the guide panel's top-left corner.
/// It is modeled on the reference design's own back chevron. It
/// returns to `ChecklistWindow`, without waiting for this permission
/// to be granted first. It uses a real SF Symbol (`chevron.left`),
/// not a Unicode `‹` glyph, matching HIG's own back-button convention.
private final class BackButton: NSButton {
    private let onTapped: () -> Void

    init(onTapped: @escaping () -> Void) {
        self.onTapped = onTapped
        super.init(frame: .zero)
        title = "Back"
        image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: nil)
        imagePosition = .imageLeading
        symbolConfiguration = .init(pointSize: 10, weight: .medium)
        bezelStyle = .recessed
        isBordered = false
        font = .preferredFont(forTextStyle: .caption1, options: [:])
        contentTintColor = .secondaryLabelColor
        target = self
        action = #selector(tapped)
        setAccessibilityLabel("Back")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    @objc private func tapped() {
        onTapped()
    }
}

/// Where the guide screen sits within the full list of needed
/// permissions — purely for display ("N of M"); carries no notion of
/// which ones are actually granted, since only the parent process
/// knows that.
struct GuideProgress {
    let index: Int
    let count: Int

    var text: String {
        "\(index + 1) of \(count)"
    }
}

/// A small "Previous" / "Next" button in the guide panel's nav row.
/// It was added after a live report: the only way to move between
/// permissions was detouring back through the checklist each time.
/// Each carries a real SF Symbol chevron (`chevron.left` or
/// `chevron.right`), not a Unicode `‹`/`›` glyph, matching HIG.
private final class NavButton: NSButton {
    private let onTapped: () -> Void

    init(title: String, symbolName: String, leading: Bool, onTapped: @escaping () -> Void) {
        self.onTapped = onTapped
        super.init(frame: .zero)
        self.title = title
        image = NSImage(systemSymbolName: symbolName, accessibilityDescription: nil)
        imagePosition = leading ? .imageLeading : .imageTrailing
        symbolConfiguration = .init(pointSize: 11, weight: .regular)
        bezelStyle = .rounded
        font = .preferredFont(forTextStyle: .caption1, options: [:])
        target = self
        action = #selector(tapped)
        setAccessibilityLabel(title)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    @objc private func tapped() {
        onTapped()
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
// `.accessory`: the helper never gets a Dock icon or appears in the
// Cmd-Tab switcher. `ChecklistWindow` still activates the app
// (`NSApp.activate(ignoringOtherApps:)`) when it's the window on
// screen, since it has nothing to coexist with yet — only the guide
// panel and success panel stay strictly non-activating, per PINV-62.
app.setActivationPolicy(.accessory)
app.run()
