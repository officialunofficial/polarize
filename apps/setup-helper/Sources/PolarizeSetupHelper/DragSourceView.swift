// The helper's "drag me" affordance (PLZ-8): shows Polarize's own app
// icon and starts a Finder-style drag session carrying its bundle path,
// so a user can drop it straight into System Settings' Accessibility or
// Screen Recording list instead of hunting for Polarize.app themselves.
// All payload data comes from `SetupHelperCore.DragPayload`, a pure
// value built from the `--for-bundle` argv (see
// `SetupHelperCore/DragPayload.swift`); this file only wires that data
// to real `NSPasteboard`/`NSDraggingSession` APIs.
//
// This file calls no TCC-adjacent API: `NSPasteboard` writing and
// `NSWorkspace.shared.icon(forFile:)` are not permission-gated calls —
// they read/write pasteboard and icon data, never a Privacy &
// Security grant. `main.swift`'s "exactly one TCC-adjacent API" header
// comment stays true because the drag code lives here, in a separate
// file, and adds no permission call of its own. See PINV-58 through
// PINV-60 in `docs/INVARIANTS.md`.
import AppKit
import SetupHelperCore

/// Wraps a pure `DragPayload` as an `NSPasteboardWriting` item, so a
/// drag session can hand it straight to `NSDraggingItem`.
final class DragPayloadPasteboardWriter: NSObject, NSPasteboardWriting {
    private let payload: DragPayload

    init(payload: DragPayload) {
        self.payload = payload
    }

    func writableTypes(for pasteboard: NSPasteboard) -> [NSPasteboard.PasteboardType] {
        payload.representations.map { NSPasteboard.PasteboardType(rawValue: $0.rawType) }
    }

    func pasteboardPropertyList(forType type: NSPasteboard.PasteboardType) -> Any? {
        guard let representation = payload.representations.first(where: { $0.rawType == type.rawValue }) else {
            return nil
        }
        switch representation.value {
        case .string(let value):
            return value
        case .stringArray(let values):
            return values
        }
    }
}

/// Shows Polarize's own app icon plus a short caption, and starts a
/// drag session carrying `payload` when the user drags from it.
final class AppIconDragView: NSView, NSDraggingSource {
    private let payload: DragPayload
    private let imageView: NSImageView

    init(payload: DragPayload, bundlePath: String) {
        self.payload = payload
        imageView = NSImageView(image: HelperIconLoader.icon(forBundleAt: bundlePath))
        super.init(frame: .zero)

        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.setAccessibilityLabel("Polarize app icon, drag to grant access")

        let caption = NSTextField(labelWithString: "Drag to the list below")
        caption.translatesAutoresizingMaskIntoConstraints = false
        caption.font = .preferredFont(forTextStyle: .caption1, options: [:])
        caption.textColor = .secondaryLabelColor
        caption.alignment = .center

        addSubview(imageView)
        addSubview(caption)
        NSLayoutConstraint.activate([
            imageView.topAnchor.constraint(equalTo: topAnchor),
            imageView.centerXAnchor.constraint(equalTo: centerXAnchor),
            imageView.widthAnchor.constraint(equalToConstant: 64),
            imageView.heightAnchor.constraint(equalToConstant: 64),
            caption.topAnchor.constraint(equalTo: imageView.bottomAnchor, constant: 4),
            caption.centerXAnchor.constraint(equalTo: centerXAnchor),
            caption.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    /// Claims every point inside this view's own bounds for itself,
    /// rather than letting hit-testing resolve to `imageView`/the
    /// caption label. Without this override, a click inside the icon
    /// area hit-tests to the child `NSImageView`, which has no drag
    /// override of its own, so `mouseDragged` below never fires — the
    /// view that actually received the mouse-down was never this one.
    /// A real, necessary fix, but on its own not sufficient to make a
    /// live drag succeed — see `onDragStateChange` below for the other
    /// half.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let localPoint = convert(point, from: superview)
        return bounds.contains(localPoint) ? self : nil
    }

    /// Lets a click register on the very first click, even though the
    /// panel that hosts this view can never become key
    /// (`FloatingHelperPanel.canBecomeKey == false`, by design — see
    /// its own doc comment). Without this override, AppKit swallows the
    /// first click on a non-key window as an activation click and never
    /// delivers it to this view at all.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        true
    }

    /// Notifies the caller when a drag session starts and ends, so
    /// `main.swift` can make the panel mouse-transparent and drop it
    /// behind other windows for the duration — see `draggingSession`
    /// below. Modeled directly on `jaywcjlove/PermissionFlow`'s
    /// `AppDragSourceView.onDragStateChange`, the open-source reference
    /// this drag mechanism was built from: its own panel calls this
    /// exact pattern "drag-friendly mode." Without it, the panel stays
    /// frontmost and mouse-opaque through the whole drag, and can
    /// intercept the drop meant for System Settings underneath it —
    /// the likely reason a live drag did not work even after the
    /// `hitTest`/`acceptsFirstMouse` fixes above landed.
    var onDragStateChange: ((Bool) -> Void)?

    /// Fired once, when a drag session ends. This means attempted, not
    /// necessarily successful: this view has no way to know whether
    /// the drop actually granted anything. Only the parent process's
    /// own TCC re-read can know that (PINV-56). `main.swift` uses this
    /// to reveal the guide screen's Next button, only once the user
    /// has actually tried the drag. That stops them skipping ahead
    /// before attempting the current step.
    var onDragCompleted: (() -> Void)?

    private var mouseDownPoint: NSPoint?
    private var hasBegunDragging = false

    override func mouseDown(with event: NSEvent) {
        mouseDownPoint = convert(event.locationInWindow, from: nil)
        hasBegunDragging = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard !hasBegunDragging, let mouseDownPoint else { return }
        let currentPoint = convert(event.locationInWindow, from: nil)
        let distance = hypot(currentPoint.x - mouseDownPoint.x, currentPoint.y - mouseDownPoint.y)
        guard distance > 4 else { return }

        hasBegunDragging = true
        let writer = DragPayloadPasteboardWriter(payload: payload)
        let draggingItem = NSDraggingItem(pasteboardWriter: writer)
        draggingItem.setDraggingFrame(imageView.frame, contents: imageView.image)
        let session = beginDraggingSession(with: [draggingItem], event: event, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
    }

    override func mouseUp(with event: NSEvent) {
        mouseDownPoint = nil
        hasBegunDragging = false
    }

    func draggingSession(
        _ session: NSDraggingSession,
        sourceOperationMaskFor context: NSDraggingContext
    ) -> NSDragOperation {
        switch context {
        case .outsideApplication:
            return [.copy, .generic]
        case .withinApplication:
            return []
        @unknown default:
            return []
        }
    }

    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool {
        true
    }

    func draggingSession(_ session: NSDraggingSession, willBeginAt screenPoint: NSPoint) {
        onDragStateChange?(true)
    }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        onDragStateChange?(false)
        onDragCompleted?()
        mouseDownPoint = nil
        hasBegunDragging = false
    }
}
