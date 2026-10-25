import AppKit

/// `quota-menubar --render <report.json> <out.png> [--dark] [--expand <account id>] [--lanes <lanes.json>]
/// [--now <ISO time>] [--scale <factor>]` draws the menu from a saved report into a PNG, offscreen, without running
/// the CLI, at 2x as on a Retina display unless told otherwise.
/// AppKit draws a real `NSMenu` only while it tracks the mouse, so the PNG is a preview at macOS 26 menu metrics.
/// It is built from the same `QuotaMenu.entries` as the menu, with the same titles, icons and information views
/// (MenuStyle.swift); only the native items' frame is imitated. `--expand` shows that account's submenu beside its
/// row. The clock defaults to the report's `generated_unix_seconds`, so a fixture draws the same way on any day.
@MainActor
enum Render {
    static func run(_ arguments: [String]) -> Int32 {
        var positional: [String] = [], dark = false, expand: String?, lanesPath: String?, now: Date?, scale = 2
        var rest = arguments[...]
        while let argument = rest.popFirst() {
            switch argument {
            case "--dark": dark = true
            case "--expand": expand = rest.popFirst()
            case "--lanes": lanesPath = rest.popFirst()
            case "--now": now = rest.popFirst().flatMap { try? Date($0, strategy: .iso8601) }
            case "--scale": scale = rest.popFirst().flatMap(Int.init).map { max(1, min($0, 4)) } ?? scale
            default: positional.append(argument)
            }
        }
        guard positional.count == 2 else {
            FileHandle.standardError.write(Data("usage: quota-menubar --render <report.json> <out.png> [--dark] [--expand id] [--lanes file] [--now ISO] [--scale N]\n".utf8))
            return 2
        }
        let report: QuotaReport, lanes: [Lane]
        do {
            report = try CLI.decode(Data(contentsOf: URL(fileURLWithPath: positional[0]))).get()
            lanes = try lanesPath.map { try Lane.decodePage(Data(contentsOf: URL(fileURLWithPath: $0))).lanes } ?? []
        } catch {
            FileHandle.standardError.write(Data("cannot read input: \(error)\n".utf8))
            return 1
        }
        let clock = now ?? report.generatedAt
        let entries = QuotaMenu.entries(for: QuotaModel(fixture: report, updatedAt: clock, lanes: lanes), now: clock)
            .filter { if case .item(let spec) = $0 { !spec.hidden } else { true } }

        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        NSApp.appearance = appearance
        let preview = MenuPreview(entries: entries, expand: expand)
        preview.appearance = appearance
        let size = preview.bounds.size
        guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width) * scale, pixelsHigh: Int(size.height) * scale,
                                            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return 1 }
        bitmap.size = size
        appearance?.performAsCurrentDrawingAppearance { preview.cacheDisplay(in: preview.bounds, to: bitmap) }
        guard let png = bitmap.representation(using: .png, properties: [:]) else { return 1 }
        do { try png.write(to: URL(fileURLWithPath: positional[1])) } catch {
            FileHandle.standardError.write(Data("cannot write \(positional[1]): \(error)\n".utf8))
            return 1
        }
        print("\(positional[1]) \(Int(size.width))x\(Int(size.height))")
        return 0
    }
}

/// The macOS 26 menu frame the preview imitates, measured from real menus: 5-point vertical padding, 24-point
/// rows (36 with a subtitle), icons centred 22.5 points in, titles at 37 points (16 without an icon), trailing
/// shortcuts and submenu arrows 18 points from the edge, and submenus overlapping their menu by 4 points.
enum MenuMetrics {
    static let padding: CGFloat = 5
    static let row: CGFloat = 24, tallRow: CGFloat = 36, header: CGFloat = 26, separator: CGFloat = 11
    static let iconCentre: CGFloat = 22.5, iconTitle: CGFloat = 37
    static let trailing: CGFloat = 18
    static let arrowColumn: CGFloat = 34          // after the title column, when an item has a submenu
    static let shortcutColumn: CGFloat = 62       // after the title column, when an item has a shortcut
    static let submenuOverlap: CGFloat = 4
    static let corner: CGFloat = 12
}

/// The menu and, when asked, one account's submenu beside its row, on a desktop-coloured backdrop.
final class MenuPreview: NSView {
    private let margin: CGFloat = 14
    private var panels: [NSRect] = []
    override var isFlipped: Bool { true }

    init(entries: [MenuEntry], expand: String?) {
        super.init(frame: .zero)
        autoresizesSubviews = false
        let main = PreviewMenu(entries: entries, open: expand)
        main.setFrameOrigin(NSPoint(x: margin, y: margin))
        addSubview(main)
        panels = [main.frame]
        var extent = NSPoint(x: main.frame.maxX, y: main.frame.maxY)
        if let (submenu, row) = main.openSubmenu {
            let sub = PreviewMenu(entries: submenu, open: nil)
            sub.setFrameOrigin(NSPoint(x: main.frame.maxX - MenuMetrics.submenuOverlap,
                                       y: main.frame.minY + row.minY - MenuMetrics.padding))
            addSubview(sub)
            panels.append(sub.frame)
            extent = NSPoint(x: max(extent.x, sub.frame.maxX), y: max(extent.y, sub.frame.maxY))
        }
        setFrameSize(NSSize(width: extent.x + margin, height: extent.y + margin))
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    override func draw(_: NSRect) {
        NSColor.underPageBackgroundColor.setFill()
        bounds.fill()
        for panel in panels {
            let path = NSBezierPath(roundedRect: panel, xRadius: MenuMetrics.corner, yRadius: MenuMetrics.corner)
            NSGraphicsContext.saveGraphicsState()
            let shadow = NSShadow()
            shadow.shadowColor = .black.withAlphaComponent(0.25)
            shadow.shadowBlurRadius = 14
            shadow.shadowOffset = NSSize(width: 0, height: -5)
            shadow.set()
            NSColor.windowBackgroundColor.setFill()
            path.fill()
            NSGraphicsContext.restoreGraphicsState()
            NSColor.separatorColor.setStroke()
            path.lineWidth = 0.5
            path.stroke()
        }
    }
}

/// One menu: rows stacked at standard metrics, with the menu's own information views and imitations of its
/// native items, separators and section headers.
final class PreviewMenu: NSView {
    private(set) var openSubmenu: ([MenuEntry], NSRect)?
    override var isFlipped: Bool { true }

    init(entries: [MenuEntry], open: String?) {
        super.init(frame: .zero)
        autoresizesSubviews = false
        let specs = entries.compactMap { if case .item(let spec) = $0 { spec } else { nil } }
        let column = MenuStyle.column(for: specs)
        let shortcuts = specs.contains { $0.shortcut != nil }, arrows = specs.contains { $0.submenu != nil }
        let trailing = shortcuts ? MenuMetrics.shortcutColumn : arrows ? MenuMetrics.arrowColumn : MenuMetrics.trailing
        var width = MenuInfoView.width
        for spec in specs {
            let lead = spec.icon == nil ? MenuStyle.inset : MenuMetrics.iconTitle
            let title = spec.icon == nil ? MenuStyle.width(NSAttributedString(string: spec.title, attributes: [.font: MenuStyle.titleFont])) : column
            width = max(width, lead + title + trailing)
        }
        var rows: [NSView] = [], y = MenuMetrics.padding
        for entry in entries {
            let view: NSView
            switch entry {
            case .separator: view = PreviewSeparator(height: MenuMetrics.separator)
            case .header(let title): view = PreviewHeader(title, height: MenuMetrics.header)
            case .item(let spec):
                let item = PreviewItem(spec: spec, column: column, open: spec.id != nil && spec.id == open)
                if item.open, let submenu = spec.submenu { openSubmenu = (submenu, NSRect(x: 0, y: y, width: width, height: item.frame.height)) }
                view = item
            default: view = MenuInfoView.make(for: entry, reusing: nil) ?? NSView()
            }
            view.frame = NSRect(x: 0, y: y, width: width, height: view.frame.height)
            y += view.frame.height
            rows.append(view)
        }
        rows.forEach(addSubview)
        setFrameSize(NSSize(width: width, height: y + MenuMetrics.padding))
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}

private final class PreviewSeparator: NSView {
    override var isFlipped: Bool { true }
    init(height: CGFloat) { super.init(frame: NSRect(x: 0, y: 0, width: 1, height: height)) }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    override func draw(_: NSRect) {
        NSColor.separatorColor.setFill()
        NSRect(x: MenuStyle.inset, y: bounds.midY - 0.5, width: bounds.width - 2 * MenuStyle.inset, height: 1).fill()
    }
}

private final class PreviewHeader: NSView {
    private let title: NSAttributedString
    override var isFlipped: Bool { true }
    init(_ title: String, height: CGFloat) {
        self.title = MenuInfoView.text(title, .systemFont(ofSize: 12, weight: .semibold), .secondaryLabelColor)
        super.init(frame: NSRect(x: 0, y: 0, width: 1, height: height))
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    override func draw(_: NSRect) {
        title.draw(with: NSRect(x: MenuStyle.inset, y: bounds.height - 21, width: bounds.width - 2 * MenuStyle.inset, height: 16),
                   options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }
}

/// A native menu item as macOS 26 draws it, from the same attributed title, subtitle and icon as the real item.
/// The row whose submenu is open has the system's quiet open-submenu highlight.
private final class PreviewItem: NSView {
    let spec: MenuItemSpec, open: Bool
    private let title: NSAttributedString
    private static let chevron = NSImage(systemSymbolName: "chevron.right", accessibilityDescription: nil)?
        .withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))
    override var isFlipped: Bool { true }

    init(spec: MenuItemSpec, column: CGFloat, open: Bool) {
        (self.spec, self.open) = (spec, open)
        title = MenuStyle.attributedTitle(spec, column: column)
            ?? NSAttributedString(string: spec.title, attributes: [.font: MenuStyle.titleFont, .foregroundColor: NSColor.labelColor])
        super.init(frame: NSRect(x: 0, y: 0, width: 1, height: spec.subtitle == nil ? MenuMetrics.row : MenuMetrics.tallRow))
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    override func draw(_: NSRect) {
        if open {
            NSColor.labelColor.withAlphaComponent(0.08).setFill()
            NSBezierPath(roundedRect: bounds.insetBy(dx: 5, dy: 0), xRadius: 8, yRadius: 8).fill()
        }
        let context = NSGraphicsContext.current?.cgContext
        context?.saveGState()
        if !spec.enabled { context?.setAlpha(0.4) }
        let lead = spec.icon == nil ? MenuStyle.inset : MenuMetrics.iconTitle
        if let image = spec.icon.flatMap(MenuStyle.image) {
            let rect = NSRect(x: MenuMetrics.iconCentre - image.size.width / 2, y: (bounds.height - image.size.height) / 2,
                              width: image.size.width, height: image.size.height)
            if image.isTemplate { tinted(image, .labelColor).draw(in: rect) } else { image.draw(in: rect) }
        }
        let right = bounds.width - MenuMetrics.trailing
        title.draw(with: NSRect(x: lead, y: spec.subtitle == nil ? 3.5 : 2.5, width: right - lead, height: 17),
                   options: [.usesLineFragmentOrigin])
        if let subtitle = spec.subtitle {
            MenuInfoView.text(subtitle, MenuStyle.subtitleFont, .secondaryLabelColor)
                .draw(with: NSRect(x: lead, y: 18.5, width: right - lead, height: 15), options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
        if let shortcut = spec.shortcut {
            MenuInfoView.text("⌘\(shortcut.uppercased())", MenuStyle.titleFont, .tertiaryLabelColor, alignment: .right)
                .draw(with: NSRect(x: right - 40, y: 3.5, width: 40, height: 17), options: [.usesLineFragmentOrigin])
        }
        if spec.submenu != nil, let chevron = Self.chevron {
            tinted(chevron, .secondaryLabelColor).draw(in: NSRect(x: right - chevron.size.width, y: (bounds.height - chevron.size.height) / 2,
                                                                  width: chevron.size.width, height: chevron.size.height))
        }
        context?.restoreGState()
    }

    private func tinted(_ image: NSImage, _ color: NSColor) -> NSImage {
        NSImage(size: image.size, flipped: false) { box in
            image.draw(in: box)
            color.set()
            box.fill(using: .sourceAtop)
            return true
        }
    }
}
