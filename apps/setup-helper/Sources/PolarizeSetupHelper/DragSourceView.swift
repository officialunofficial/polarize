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
        imageView = NSImageView(image: NSWorkspace.shared.icon(forFile: bundlePath))
        super.init(frame: .zero)

        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.imageScaling = .scaleProportionallyUpOrDown

        let caption = NSTextField(labelWithString: "Drag to the list below")
        caption.translatesAutoresizingMaskIntoConstraints = false
        caption.font = .systemFont(ofSize: 11)
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

    override func mouseDragged(with event: NSEvent) {
        let writer = DragPayloadPasteboardWriter(payload: payload)
        let draggingItem = NSDraggingItem(pasteboardWriter: writer)
        draggingItem.setDraggingFrame(imageView.frame, contents: imageView.image)
        beginDraggingSession(with: [draggingItem], event: event, source: self)
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
}
