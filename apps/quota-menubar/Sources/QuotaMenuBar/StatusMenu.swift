import AppKit

/// The status item and its standard `NSMenu`: system highlighting, keyboard navigation, section headers and
/// submenus come from AppKit. The menu is rebuilt from `QuotaMenu.entries` as it opens, and updated in place while
/// it stays open, so an open submenu survives a refresh.
@MainActor
final class StatusMenu: NSObject, NSMenuDelegate {
    private let model: QuotaModel
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let menu = NSMenu()
    private var isOpen = false
    private var ticker: Timer?   // keeps relative times current while the menu is open
    private var generation = 0   // only the latest observation rebuilds

    init(model: QuotaModel) {
        self.model = model
        super.init()
        menu.delegate = self
        menu.autoenablesItems = false
        item.menu = menu
        observeLabel()
    }

    // MARK: Status item

    /// The status item follows the model's settled label, which changes only when what it shows changes.
    private func observeLabel() {
        let label = withObservationTracking { model.label } onChange: { [weak self] in
            Task { @MainActor in self?.observeLabel() }
        }
        guard let button = item.button else { return }
        button.image = label.usesBrandGlyph ? Brand.statusGlyph
            : NSImage(systemSymbolName: label.symbol, accessibilityDescription: nil)
        button.imagePosition = label.text == nil ? .imageOnly : .imageLeading
        button.font = .monospacedDigitSystemFont(ofSize: NSFont.menuBarFont(ofSize: 0).pointSize, weight: .regular)
        button.title = label.text ?? ""
        button.setAccessibilityLabel(label.spoken)
    }

    // MARK: Menu

    func menuNeedsUpdate(_ menu: NSMenu) { if menu === self.menu { update() } }

    func menuWillOpen(_ menu: NSMenu) {
        guard menu === self.menu else { return }
        isOpen = true
        model.panelOpened()
        let tick = Timer(timeInterval: 30, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.update() } }
        RunLoop.main.add(tick, forMode: .common)   // .common includes menu tracking
        ticker = tick
    }

    func menuDidClose(_ menu: NSMenu) {
        guard menu === self.menu else { return }
        isOpen = false
        ticker?.invalidate()
        ticker = nil
    }

    /// Builds the entries, watching the model so a change while the menu is open shows at once.
    private func update() {
        generation += 1
        let current = generation
        let entries = withObservationTracking { QuotaMenu.entries(for: model, now: .now) } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, self.isOpen, self.generation == current else { return }
                self.update()
            }
        }
        sync(menu, entries)
    }

    /// Updates items in place when the shape is unchanged, and rebuilds the menu otherwise.
    private func sync(_ menu: NSMenu, _ entries: [MenuEntry]) {
        let same = menu.items.count == entries.count && zip(menu.items, entries).allSatisfy { item, entry in
            switch entry {
            case .separator: item.isSeparatorItem
            case .header: item.isSectionHeader
            case .item: !item.isSeparatorItem && !item.isSectionHeader
            }
        }
        if !same {
            menu.removeAllItems()
            for entry in entries {
                switch entry {
                case .separator: menu.addItem(.separator())
                case .header(let title): menu.addItem(.sectionHeader(title: title))
                case .item: menu.addItem(NSMenuItem())
                }
            }
        }
        for (item, entry) in zip(menu.items, entries) {
            switch entry {
            case .separator: break
            case .header(let title): if item.title != title { item.title = title }
            case .item(let spec): configure(item, spec)
            }
        }
    }

    private func configure(_ item: NSMenuItem, _ spec: MenuItemSpec) {
        if #available(macOS 14.4, *) {
            item.title = spec.title
            item.subtitle = spec.subtitle
        } else {
            item.title = [spec.title, spec.subtitle].compactMap { $0 }.joined(separator: " · ")
        }
        item.badge = spec.badge.map { NSMenuItemBadge(string: $0) }
        item.image = spec.symbol.flatMap { Self.image($0, spec.tint) }
        item.keyEquivalent = spec.shortcut ?? ""
        item.keyEquivalentModifierMask = .command
        item.toolTip = spec.help
        item.representedObject = spec.action.map(ActionBox.init)
        item.target = spec.action == nil ? nil : self
        item.action = spec.action == nil ? nil : #selector(choose(_:))
        item.isEnabled = spec.enabled
        if let entries = spec.submenu {
            let submenu = item.submenu ?? {
                let submenu = NSMenu()
                submenu.autoenablesItems = false
                return submenu
            }()
            sync(submenu, entries)
            if item.submenu !== submenu { item.submenu = submenu }
        } else if item.submenu != nil {
            item.submenu = nil
        }
    }

    @objc private func choose(_ sender: NSMenuItem) {
        guard let action = (sender.representedObject as? ActionBox)?.action else { return }
        switch action {
        case .refresh: model.refresh()
        case .quit: NSApplication.shared.terminate(nil)
        case .open(let url): NSWorkspace.shared.open(url)
        }
    }

    /// An SF Symbol in a semantic system colour; untinted symbols stay templates and follow the menu's text colour.
    static func image(_ name: String, _ tint: MenuTint?) -> NSImage? {
        guard let image = NSImage(systemSymbolName: name, accessibilityDescription: nil) else { return nil }
        guard let tint else { return image }
        return image.withSymbolConfiguration(.init(hierarchicalColor: tint.color))
    }
}

private final class ActionBox: NSObject {
    let action: MenuAction
    init(_ action: MenuAction) { self.action = action }
}

extension MenuTint {
    var color: NSColor {
        switch self {
        case .green: .systemGreen
        case .orange: .systemOrange
        case .red: .systemRed
        case .blue: .systemBlue
        case .secondary: .secondaryLabelColor
        }
    }
}
