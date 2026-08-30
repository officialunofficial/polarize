// The impure half of PLZ-7's window tracking: resolves System
// Settings' running process(es), reads its window bounds through
// either `CGWindowListCopyWindowInfo` or an `AXObserver`, and keeps
// the helper's floating panel positioned relative to it. All the pure
// decisions this file makes — which window is System Settings' own,
// where the panel's frame goes, which tracking mechanism to use — live
// in `SetupHelperCore.WindowTracking` and are unit-tested there. This
// file only wires that logic to the real APIs. See PINV-62 in
// `docs/INVARIANTS.md`.
import AppKit
import ApplicationServices
import CoreGraphics
import SetupHelperCore

/// Keeps `panel` positioned relative to System Settings' own window,
/// by polling `CGWindowListCopyWindowInfo` or, once the strategy
/// picker chooses to, observing System Settings' window directly via
/// `AXObserver`.
///
/// The `AXObserver` path needs the *helper's own* process to already
/// hold Accessibility trust. This feature has no way to grant that —
/// PINV-58 forbids the helper requesting its own trust, and the whole
/// point of `--request-permissions` is granting Accessibility to
/// *Polarize's* bundle, not the helper's — so in practice this class
/// almost always ends up on the polling path. The observer path stays
/// in place because `TrackingStrategyPicker` is written to prefer it
/// whenever it legitimately can, and because a user could plausibly
/// have granted the helper's own identity Accessibility by hand.
@MainActor
final class WindowTracker {
    private let panel: NSPanel
    private let panelSize: NSSize
    private var pollTimer: Timer?
    private var axObserver: AXObserver?

    /// Set while a drag session is in progress (PLZ-8's
    /// `AppIconDragView`), so the tracker skips repositioning the panel.
    /// A `Timer` added in `.common` run-loop mode — needed so tracking
    /// keeps working while the user has, say, a menu open — also keeps
    /// firing during `NSDraggingSession`'s own event-tracking loop, so
    /// without this guard the panel gets repositioned out from under an
    /// in-progress drag every poll interval. Suspected contributor to a
    /// live "drag doesn't work" report; not isolated as the sole cause,
    /// so this is a real fix either way, not just a hedge.
    private var isSuspended = false

    /// How often the polling path re-reads the window list. Fast
    /// enough that a dragged System Settings window feels tracked, not
    /// laggy; slow enough to cost nothing noticeable.
    private static let pollInterval: TimeInterval = 0.2

    /// How many `pollInterval`-spaced attempts to make before giving up
    /// on finding System Settings' process. `LaunchPlanner` opens it
    /// moments before `start()` runs, so this covers ordinary launch
    /// latency, not an indefinite wait.
    private static let pidResolutionAttempts = 15

    init(panel: NSPanel, panelSize: NSSize) {
        self.panel = panel
        self.panelSize = panelSize
    }

    /// Starts tracking. Safe to call once; call `stop()` before
    /// tearing the helper down.
    func start() {
        resolveSettingsPIDs(attemptsLeft: Self.pidResolutionAttempts) { [weak self] pids in
            guard let self else { return }
            guard !pids.isEmpty else {
                self.applyFallbackFrame()
                return
            }
            switch TrackingStrategyPicker.pick(helperIsAXTrusted: AXIsProcessTrusted()) {
            case .cgWindowPolling:
                self.startPolling(pids: pids)
            case .axObserver:
                if !self.startObserving(pid: pids[0]) {
                    self.startPolling(pids: pids)
                }
            }
        }
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        axObserver = nil
    }

    /// Stops repositioning the panel until `resume()`. Does not stop
    /// the underlying timer/observer — only the frame writes they'd
    /// otherwise trigger — so tracking resumes exactly where it left
    /// off once the drag ends.
    func suspend() {
        isSuspended = true
    }

    func resume() {
        isSuspended = false
    }

    // MARK: - Resolving System Settings' process

    private func resolveSettingsPIDs(attemptsLeft: Int, completion: @escaping ([Int]) -> Void) {
        let pids = NSRunningApplication
            .runningApplications(withBundleIdentifier: SystemSettings.bundleIdentifier)
            .map { Int($0.processIdentifier) }
        if !pids.isEmpty || attemptsLeft <= 0 {
            completion(pids)
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.pollInterval) { [weak self] in
            self?.resolveSettingsPIDs(attemptsLeft: attemptsLeft - 1, completion: completion)
        }
    }

    // MARK: - CGWindowList polling (PINV-62's no-permission path)

    private func startPolling(pids: [Int]) {
        let owned = Set(pids)
        applyFrame(forPIDs: owned)
        let timer = Timer.scheduledTimer(withTimeInterval: Self.pollInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.applyFrame(forPIDs: owned)
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    private func applyFrame(forPIDs pids: Set<Int>) {
        guard
            let windows = Self.currentWindows(),
            let settings = SettingsWindowLocator.pick(from: windows, ownedByPIDs: pids)
        else {
            applyFallbackFrame()
            return
        }
        applyFrame(overSettingsBounds: settings.bounds)
    }

    private static func currentWindows() -> [TrackedWindowInfo]? {
        let options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
        guard
            let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: AnyObject]]
        else {
            return nil
        }
        return list.compactMap(trackedWindow(from:))
    }

    private static func trackedWindow(from entry: [String: AnyObject]) -> TrackedWindowInfo? {
        guard
            let ownerPID = entry[kCGWindowOwnerPID as String] as? Int,
            let layer = entry[kCGWindowLayer as String] as? Int
        else {
            return nil
        }
        let bounds = entry[kCGWindowBounds as String]
            .flatMap { CGRect(dictionaryRepresentation: $0 as! CFDictionary) } ?? .zero  // swiftlint:disable:this force_cast
        // Absent from the dictionary means "not on the Space in front
        // of the user" — see `crates/polarize-macos/src/workspace.rs`'s
        // module doc comment for the same fact on the Rust side.
        let isOnScreen = (entry[kCGWindowIsOnscreen as String] as? Bool) ?? false
        return TrackedWindowInfo(ownerPID: ownerPID, layer: layer, isOnScreen: isOnScreen, bounds: bounds)
    }

    // MARK: - AXObserver (rarely reachable — see class doc comment)

    private func startObserving(pid: Int) -> Bool {
        let axPID = pid_t(pid)
        var observer: AXObserver?
        let callback: AXObserverCallback = { _, element, _, refcon in
            guard let refcon else { return }
            let tracker = Unmanaged<WindowTracker>.fromOpaque(refcon).takeUnretainedValue()
            MainActor.assumeIsolated {
                tracker.applyFrame(fromAXWindow: element)
            }
        }
        guard AXObserverCreate(axPID, callback, &observer) == .success, let observer else {
            return false
        }

        let appElement = AXUIElementCreateApplication(axPID)
        var windowValue: AnyObject?
        guard
            AXUIElementCopyAttributeValue(appElement, kAXFocusedWindowAttribute as CFString, &windowValue)
                == .success,
            let windowValue,
            CFGetTypeID(windowValue) == AXUIElementGetTypeID()
        else {
            return false
        }
        let window = windowValue as! AXUIElement  // swiftlint:disable:this force_cast

        let refcon = Unmanaged.passUnretained(self).toOpaque()
        AXObserverAddNotification(observer, window, kAXMovedNotification as CFString, refcon)
        AXObserverAddNotification(observer, window, kAXResizedNotification as CFString, refcon)
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .defaultMode)

        axObserver = observer
        applyFrame(fromAXWindow: window)
        return true
    }

    private func applyFrame(fromAXWindow element: AXUIElement) {
        var positionValue: AnyObject?
        var sizeValue: AnyObject?
        guard
            AXUIElementCopyAttributeValue(element, kAXPositionAttribute as CFString, &positionValue) == .success,
            AXUIElementCopyAttributeValue(element, kAXSizeAttribute as CFString, &sizeValue) == .success,
            let positionValue, let sizeValue
        else {
            return
        }
        var origin = CGPoint.zero
        var size = CGSize.zero
        AXValueGetValue(positionValue as! AXValue, .cgPoint, &origin)  // swiftlint:disable:this force_cast
        AXValueGetValue(sizeValue as! AXValue, .cgSize, &size)  // swiftlint:disable:this force_cast
        applyFrame(overSettingsBounds: CGRect(origin: origin, size: size))
    }

    // MARK: - Shared frame application

    private func applyFrame(overSettingsBounds bounds: CGRect) {
        guard !isSuspended else { return }
        guard let screen = NSScreen.screens.first else { return }
        let frame = PanelFramePlanner.frame(
            overSettingsBounds: bounds,
            screenWidth: screen.frame.width,
            screenHeight: screen.frame.height,
            panelSize: panelSize
        )
        panel.setFrame(frame, display: true)
    }

    private func applyFallbackFrame() {
        guard !isSuspended else { return }
        guard let screen = NSScreen.screens.first else { return }
        let frame = PanelFramePlanner.fallbackFrame(
            screenWidth: screen.frame.width,
            screenHeight: screen.frame.height,
            panelSize: panelSize
        )
        panel.setFrame(frame, display: true)
    }
}
