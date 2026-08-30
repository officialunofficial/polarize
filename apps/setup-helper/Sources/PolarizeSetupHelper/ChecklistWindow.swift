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
        isOpaque = false
        backgroundColor = .clear
        // With no title bar strip left to drag by, the window needs
        // this explicitly or it becomes undraggable entirely.
        isMovableByWindowBackground = true

        let content = Self.makeContent(items: items, appIcon: appIcon, width: width) { [weak self] permission in
            self?.onAllowTapped?(permission)
        }
        // `cornerRadius: 0`. Unlike the floating guide panel, this is a
        // real window. It already has native rounded corners from the
        // window server. Rounding the material too would double up.
        contentView = MaterialBackground.wrap(
            content: content,
            cornerRadius: 0,
            material: .contentBackground,
            blendingMode: .behindWindow
        )
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
        card.layer?.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(0.15).cgColor
        card.layer?.cornerRadius = 12

        let resolvedIcon = PermissionIcon.resolve(for: item)
        let icon = NSImageView(image: resolvedIcon.image)
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.setAccessibilityLabel(item.title)
        if resolvedIcon.isSymbol {
            icon.contentTintColor = .controlAccentColor
            icon.symbolConfiguration = .init(pointSize: 32, weight: .regular)
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
        // Deliberately not `.glass` bezel style. `NSBezelStyleGlass`
        // exists in the macOS 27 SDK, but confirmed live: it rendered
        // as plain text, with no button chrome at all. Multiple Apple
        // Developer Forums threads (FB20272917, FB20517174) confirm
        // that specific flag is genuinely unreliable this OS cycle.
        // Below, instead, is Apple's own documented composition
        // pattern for a real Liquid Glass button: a separate
        // `NSGlassEffectView`, sized to match, sitting behind a plain
        // borderless button — not the bezel-style flag.
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
        if #available(macOS 26.0, *) {
            // Apple's own real Liquid Glass button pattern: the glass
            // view sits behind the button, sized to exactly match it,
            // and the button itself draws no bezel of its own — only
            // the glass supplies the visible pill shape underneath.
            // `positioned: .below, relativeTo: button` needs `button`
            // already added as a subview above, or it silently fails
            // to place the glass behind it.
            button.isBordered = false
            // With no bezel of its own, the button's default title
            // color is too dark to read against a tinted glass
            // background — set explicitly, to match the white title
            // the bezel path already gets for free from AppKit.
            button.attributedTitle = NSAttributedString(
                string: "Allow",
                attributes: [.foregroundColor: NSColor.white]
            )
            let glass = NSGlassEffectView()
            glass.cornerRadius = 8
            glass.tintColor = .controlAccentColor
            glass.translatesAutoresizingMaskIntoConstraints = false
            card.addSubview(glass, positioned: .below, relativeTo: button)
            NSLayoutConstraint.activate([
                glass.leadingAnchor.constraint(equalTo: button.leadingAnchor, constant: -10),
                glass.trailingAnchor.constraint(equalTo: button.trailingAnchor, constant: 10),
                glass.topAnchor.constraint(equalTo: button.topAnchor, constant: -4),
                glass.bottomAnchor.constraint(equalTo: button.bottomAnchor, constant: 4),
            ])
        }

        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            icon.centerYAnchor.constraint(equalTo: card.centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 44),
            icon.heightAnchor.constraint(equalToConstant: 44),

            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 14),
            title.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            title.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),

            detail.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 2),
            detail.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),
            detail.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12),

            button.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),
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
