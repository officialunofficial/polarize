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
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        title = "Enable Polarize"
        isReleasedWhenClosed = false
        level = .normal

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
            NSLayoutConstraint.activate([
                iconView.widthAnchor.constraint(equalToConstant: 64),
                iconView.heightAnchor.constraint(equalToConstant: 64),
            ])
            stack.addArrangedSubview(iconView)
        }

        let heading = NSTextField(labelWithString: "Enable Polarize")
        heading.font = .boldSystemFont(ofSize: 20)
        heading.alignment = .center
        stack.addArrangedSubview(heading)

        let subheading = NSTextField(
            wrappingLabelWithString: "Polarize needs these permissions to automate apps on your Mac."
        )
        subheading.font = .systemFont(ofSize: 12)
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

        let icon = NSImageView(
            image: NSImage(systemSymbolName: item.symbolName, accessibilityDescription: item.title)
                ?? NSImage()
        )
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.contentTintColor = .controlAccentColor
        icon.symbolConfiguration = .init(pointSize: 22, weight: .regular)

        let title = NSTextField(labelWithString: item.title)
        title.font = .boldSystemFont(ofSize: 13)
        title.translatesAutoresizingMaskIntoConstraints = false

        let detail = NSTextField(wrappingLabelWithString: item.detail)
        detail.font = .systemFont(ofSize: 11)
        detail.textColor = .secondaryLabelColor
        detail.translatesAutoresizingMaskIntoConstraints = false

        let button = AllowButton(permission: item.permission, onAllow: onAllow)
        button.title = "Allow"
        button.bezelStyle = .rounded
        button.translatesAutoresizingMaskIntoConstraints = false
        if isPrimary {
            button.keyEquivalent = "\r"
        }

        card.addSubview(icon)
        card.addSubview(title)
        card.addSubview(detail)
        card.addSubview(button)

        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            icon.centerYAnchor.constraint(equalTo: card.centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 28),
            icon.heightAnchor.constraint(equalToConstant: 28),

            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 12),
            title.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            title.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),

            detail.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 2),
            detail.trailingAnchor.constraint(lessThanOrEqualTo: button.leadingAnchor, constant: -12),
            detail.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12),

            button.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
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
