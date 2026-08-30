// Shared by `ChecklistWindow` and `main.swift`'s floating guide/success
// panels. Backs a content view with a translucent, vibrant material.
// It adopts Liquid Glass on macOS 26+, via `NSGlassEffectView`. That
// API is verified against the real macOS 27 SDK headers on this
// machine. On older systems, it falls back to `NSVisualEffectView`.
import AppKit

enum MaterialBackground {
    /// - `cornerRadius`: round the material itself. Pass `0` for a real
    ///   window. It already has native rounded corners from the window
    ///   server. Rounding the content too would double up. Pass a real
    ///   radius for a borderless custom panel instead. That kind of
    ///   panel has no window-server rounding of its own.
    /// - `blendingMode`: `.behindWindow` blurs and tints whatever sits
    ///   behind the real window — the desktop, or other apps. That is
    ///   the actual "vibrant window" look. `.withinWindow` is the
    ///   pre-26 fallback's default instead. It only composites with
    ///   content behind this view, inside the same window. A small
    ///   floating panel, sitting over the app's own other windows,
    ///   wants that narrower blend instead.
    @MainActor
    static func wrap(
        content: NSView,
        cornerRadius: CGFloat,
        material: NSVisualEffectView.Material,
        blendingMode: NSVisualEffectView.BlendingMode = .withinWindow
    ) -> NSView {
        content.translatesAutoresizingMaskIntoConstraints = false
        if #available(macOS 26.0, *) {
            let glass = NSGlassEffectView()
            glass.cornerRadius = cornerRadius
            glass.contentView = content
            NSLayoutConstraint.activate([
                content.leadingAnchor.constraint(equalTo: glass.leadingAnchor),
                content.trailingAnchor.constraint(equalTo: glass.trailingAnchor),
                content.topAnchor.constraint(equalTo: glass.topAnchor),
                content.bottomAnchor.constraint(equalTo: glass.bottomAnchor),
            ])
            return glass
        }

        let backing = NSVisualEffectView()
        backing.translatesAutoresizingMaskIntoConstraints = false
        backing.material = material
        backing.blendingMode = blendingMode
        backing.state = .active
        if cornerRadius > 0 {
            backing.wantsLayer = true
            backing.layer?.cornerRadius = cornerRadius
            backing.layer?.masksToBounds = true
        }

        let container = NSView()
        container.addSubview(backing)
        container.addSubview(content)
        NSLayoutConstraint.activate([
            backing.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            backing.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            backing.topAnchor.constraint(equalTo: container.topAnchor),
            backing.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            content.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            content.topAnchor.constraint(equalTo: container.topAnchor),
            content.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
        return container
    }
}
