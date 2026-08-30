// The helper's first screen (PLZ-3 follow-up, modeled directly on
// OpenAI Codex's "Enable Codex Computer Use" onboarding): a real,
// titled, closable window — standard traffic lights, keyable, can
// become main — listing every still-needed permission as its own row
// with an icon, a title, a one-line explanation, and an "Allow"
// button. Unlike `FloatingHelperPanel`, this window has nothing to
// coexist with on screen yet (System Settings hasn't opened), so it
// behaves like an ordinary app window rather than a non-activating
// overlay. All row content comes from `SetupHelperCore.PermissionChecklist`,
// a pure permission -> (title, detail, symbol) mapping.
import AppKit
import SetupHelperCore

@MainActor
final class ChecklistWindow: NSWindow {
    /// Fired when the user clicks "Allow" on a row. The caller decides
    /// what happens next (open the pane, show the drag-guide panel) —
    /// this window only reports the click.
    var onAllowTapped: ((NeededPermission) -> Void)?

    init(items: [PermissionChecklistItem], appIcon: NSImage?) {
        let width: CGFloat = 460
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: width, height: 200),
            // `.fullSizeContentView` is required alongside
            // `titlebarAppearsTransparent`. This is confirmed live, and
            // matches Apple's own documented pattern. Without it, the
            // content view stops below the title bar's real height.
            // That strip then goes unfilled by the vibrancy backing
            // below. A screenshot caught exactly this: a disconnected
            // white gap, sitting above the traffic lights, before this
            // fix landed.
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        // A real `title` still exists, for VoiceOver, Mission Control,
        // and the Window menu. `titleVisibility` only hides its
        // on-screen text. The big "Enable Polarize" heading inside the
        // content is the window's one visible header now. It no longer
        // repeats the same words in the title bar too.
        title = "Enable Polarize"
        titleVisibility = .hidden
        titlebarAppearsTransparent = true
        isReleasedWhenClosed = false
        level = .normal
        // Barely transparent, not the fully see-through look a `.clear`
        // background plus `.behindWindow` vibrancy gave — rejected on
        // live review. This is a near-opaque solid color with a small
        // amount of alpha, not blur-compositing with the desktop.
        isOpaque = false
        backgroundColor = NSColor.windowBackgroundColor.withAlphaComponent(0.99)
        // With no title bar strip left to drag by, the window needs
        // this explicitly or it becomes undraggable entirely.
        isMovableByWindowBackground = true

        let content = Self.makeContent(items: items, appIcon: appIcon, width: width) { [weak self] permission in
            self?.onAllowTapped?(permission)
        }
        contentView = content
        let fittingHeight = content.fittingSize.height
        setContentSize(NSSize(width: width, height: max(240, fittingHeight)))
        center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    private static func makeContent(
        items: [PermissionChecklistItem],
        appIcon: NSImage?,
        width: CGFloat,
        onAllow: @escaping (NeededPermission) -> Void
    ) -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 18
        stack.edgeInsets = NSEdgeInsets(top: 28, left: 28, bottom: 28, right: 28)
        stack.translatesAutoresizingMaskIntoConstraints = false

        if let appIcon {
            let iconView = NSImageView(image: appIcon)
            iconView.translatesAutoresizingMaskIntoConstraints = false
            iconView.setAccessibilityLabel("Polarize")
            NSLayoutConstraint.activate([
                iconView.widthAnchor.constraint(equalToConstant: 64),
                iconView.heightAnchor.constraint(equalToConstant: 64),
            ])
            stack.addArrangedSubview(iconView)
        }

        let heading = NSTextField(labelWithString: "Enable Polarize")
        // Semibold, at the Dynamic Type size .title1 already picks —
        // preferredFont(forTextStyle:) alone comes back regular
        // weight, which read too light for the window's one heading.
        heading.font = .systemFont(
            ofSize: NSFont.preferredFont(forTextStyle: .title1, options: [:]).pointSize,
            weight: .semibold
        )
        heading.alignment = .center
        stack.addArrangedSubview(heading)

        let subheading = NSTextField(
            wrappingLabelWithString: "Polarize needs these permissions to automate apps on your Mac."
        )
        subheading.font = .preferredFont(forTextStyle: .subheadline, options: [:])
        subheading.textColor = .secondaryLabelColor
        subheading.alignment = .center
        subheading.preferredMaxLayoutWidth = width - 56
        stack.addArrangedSubview(subheading)

        for (index, item) in items.enumerated() {
            let row = makeRow(item: item, isPrimary: index == 0, width: width - 56, onAllow: onAllow)
            row.translatesAutoresizingMaskIntoConstraints = false
            row.widthAnchor.constraint(equalToConstant: width - 56).isActive = true
            stack.addArrangedSubview(row)
        }

        return stack
    }

    private static func makeRow(
        item: PermissionChecklistItem,
        isPrimary: Bool,
        width: CGFloat,
        onAllow: @escaping (NeededPermission) -> Void
    ) -> NSView {
        let card = NSView()
        card.wantsLayer = true
        // A flat, dynamic system color — not Liquid Glass. Rejected on
        // live review: glass rows read as too busy against a solid
        // window. `.quaternaryLabelColor` already adapts between light
        // and dark appearance on its own; no separate dark/light branch
        // is needed.
        card.layer?.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(0.08).cgColor
        card.layer?.cornerRadius = 12
        // A soft shadow under each row, not a hard drop shadow — low
        // opacity, wide radius, offset only slightly downward. This
        // sits on the row's own layer only. `AllowButton` never sets
        // any shadow of its own — its very faint bevel is just the
        // system's native `.rounded` bezel rendering, not a shadow we
        // added, and it isn't controllable from here.
        card.layer?.masksToBounds = false
        card.layer?.shadowColor = NSColor.black.cgColor
        card.layer?.shadowOpacity = 0.22
        card.layer?.shadowRadius = 8
        card.layer?.shadowOffset = CGSize(width: 0, height: -2)

        let resolvedIcon = PermissionIcon.resolve(for: item)
        let icon = NSImageView(image: resolvedIcon.image)
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.setAccessibilityLabel(item.title)
        if resolvedIcon.isSymbol {
            icon.contentTintColor = .controlAccentColor
            icon.symbolConfiguration = .init(pointSize: 42, weight: .regular)
        } else {
            icon.imageScaling = .scaleProportionallyUpOrDown
        }

        let title = NSTextField(labelWithString: item.title)
        title.font = .preferredFont(forTextStyle: .headline, options: [:])
        title.translatesAutoresizingMaskIntoConstraints = false

        let detail = NSTextField(wrappingLabelWithString: item.detail)
        detail.font = .preferredFont(forTextStyle: .caption1, options: [:])
        detail.textColor = .secondaryLabelColor
        detail.translatesAutoresizingMaskIntoConstraints = false

        let button = AllowButton(permission: item.permission, onAllow: onAllow)
        button.title = "Allow"
        // Flat, not Liquid Glass. `.rounded` is also the one bezel
        // style confirmed to render real chrome at all — `.glass`
        // rendered as plain text with none, confirmed live (see
        // PINV-67). The rows around it carry the glass look instead.
        button.bezelStyle = .rounded
        // Back to plain default sizing. `.large` controlSize, then an
        // explicit 16pt font plus a forced 38pt minimum height, were
        // each tried per live feedback wanting the button bigger —
        // the forced height also broke the rounded bezel's own
        // vertical centering of its title, since that override made
        // the frame taller than the bezel's natural height. Live
        // feedback then asked for smaller again, past even the
        // original default. Reverting to no explicit controlSize,
        // font, or height fixes both: back to a plain, correctly
        // centered button.
        // `.controlAccentColor`, not a hardcoded blue — it renders as
        // blue for the vast majority of users (macOS's own default),
        // but still respects anyone who picked a different System
        // Settings accent color, per HIG.
        button.bezelColor = .controlAccentColor
        button.translatesAutoresizingMaskIntoConstraints = false
        button.setAccessibilityLabel("Allow \(item.title)")
        if isPrimary {
            button.keyEquivalent = "\r"
        }

        card.addSubview(icon)
        card.addSubview(title)
        card.addSubview(detail)
        card.addSubview(button)

        // `rowMargin` is the one margin every edge in this card
        // shares — the icon's leading inset and the outer row width's
        // own margin in `makeContent` both use the same value.
        let rowMargin: CGFloat = 16
        // The button gets its own, slightly wider trailing margin than
        // the icon's leading one. Confirmed live: matching them exactly
        // still read as tight on the button side — a rounded pill
        // sitting against a rounded card corner reads closer than a
        // square icon does at the same numeric gap.
        let buttonMargin: CGFloat = 20
        // Vertical clearance around the row's own content. The icon is
        // 56pt tall; this must stay bigger than half that so the icon
        // never touches the card's top or bottom edge, whatever the
        // title/detail text's own height happens to be.
        let cardVerticalMargin: CGFloat = 16

        // The icon (56pt) is the row's tallest content now, so IT pins
        // the card's top and bottom — an equality on both edges, which
        // together with its fixed height fully determines the card's
        // height as `56 + cardVerticalMargin * 2`. The title/detail
        // block stays top-anchored as before; its own bottom anchor
        // below is a `<=` safety net, not a second equality, so the
        // two blocks never fight over which one decides the card's
        // height — exactly one required chain (the icon's) does.
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: rowMargin),
            icon.topAnchor.constraint(equalTo: card.topAnchor, constant: cardVerticalMargin),
            icon.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -cardVerticalMargin),
            icon.widthAnchor.constraint(equalToConstant: 56),
            icon.heightAnchor.constraint(equalToConstant: 56),

            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 14),
            title.topAnchor.constraint(equalTo: card.topAnchor, constant: cardVerticalMargin),
            title.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),

            detail.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 2),
            detail.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),
            detail.bottomAnchor.constraint(lessThanOrEqualTo: card.bottomAnchor, constant: -cardVerticalMargin),

            button.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -buttonMargin),
            button.centerYAnchor.constraint(equalTo: card.centerYAnchor),
        ])

        return card
    }
}

/// A plain `NSButton` subclass that closes over which permission it
/// represents, so one target-action pair can serve every row without
/// a separate delegate protocol.
private final class AllowButton: NSButton {
    private let permission: NeededPermission
    private let onAllow: (NeededPermission) -> Void

    init(permission: NeededPermission, onAllow: @escaping (NeededPermission) -> Void) {
        self.permission = permission
        self.onAllow = onAllow
        super.init(frame: .zero)
        target = self
        action = #selector(tapped)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    @objc private func tapped() {
        onAllow(permission)
    }
}
