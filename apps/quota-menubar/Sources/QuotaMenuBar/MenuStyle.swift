import AppKit

/// How menu entries look in AppKit, shared by the real menu (StatusMenu.swift) and the `--render` preview
/// (Render.swift): the circular icons, the item titles with their aligned trailing figure, and the information
/// views in the title row and account submenus.
@MainActor
enum MenuStyle {
    /// Leading and trailing text inset of a macOS 26 menu, which section headers and separators also use.
    static let inset: CGFloat = 16
    static let iconSize: CGFloat = 16
    static let titleFont = NSFont.menuFont(ofSize: 0)
    static let figureFont = NSFont.monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
    static let subtitleFont = NSFont.menuFont(ofSize: 12)
    static let captionFont = NSFont.systemFont(ofSize: 11)
    /// The narrowest title column, so the menu keeps its width as times and figures change.
    static let minimumColumn: CGFloat = 250
    /// Space kept between a title and its figure.
    static let figureGap: CGFloat = 20

    // MARK: Item titles

    /// The column, from the title's leading edge, at which every row's figure ends: wide enough for the widest
    /// title with its figure, and for the widest subtitle, so figures sit at the trailing edge in one column.
    static func column(for specs: [MenuItemSpec]) -> CGFloat {
        let widest = specs.filter { $0.icon != nil && !$0.hidden }.map { spec in
            max(width(head(spec)) + (spec.value.map { figureGap + width(NSAttributedString(string: $0, attributes: [.font: figureFont])) } ?? 0),
                spec.subtitle.map { width(NSAttributedString(string: $0, attributes: [.font: subtitleFont])) } ?? 0)
        }.max() ?? 0
        return max(minimumColumn, ceil(widest))
    }

    /// The title, its secondary detail and, after a right-aligned tab, its figure. Nil for a plain title, which
    /// the menu draws itself.
    static func attributedTitle(_ spec: MenuItemSpec, column: CGFloat) -> NSAttributedString? {
        guard spec.detail != nil || spec.value != nil else { return nil }
        let paragraph = NSMutableParagraphStyle()
        paragraph.tabStops = [NSTextTab(textAlignment: .right, location: column)]
        paragraph.lineBreakMode = .byClipping
        let title = NSMutableAttributedString(attributedString: head(spec))
        if let value = spec.value {
            let color: NSColor = !spec.enabled ? .tertiaryLabelColor : spec.dimValue ? .secondaryLabelColor : .labelColor
            title.append(NSAttributedString(string: "\t" + value, attributes: [.font: figureFont, .foregroundColor: color]))
        }
        title.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: title.length))
        return title
    }

    private static func head(_ spec: MenuItemSpec) -> NSAttributedString {
        // Explicit colours, as an attributed title gets no default; highlighting still turns every run white.
        let head = NSMutableAttributedString(string: spec.title, attributes: [
            .font: titleFont, .foregroundColor: spec.enabled ? NSColor.labelColor : NSColor.tertiaryLabelColor])
        if let detail = spec.detail {
            head.append(NSAttributedString(string: " " + detail,
                                           attributes: [.font: titleFont, .foregroundColor: NSColor.secondaryLabelColor]))
        }
        return head
    }

    static func width(_ text: NSAttributedString) -> CGFloat { ceil(text.size().width) }

    /// What VoiceOver reads for a row: its title, detail, figure and subtitle, without the tab.
    static func spoken(_ spec: MenuItemSpec) -> String {
        [spec.title, spec.detail, spec.value.map { "\($0) used" }, spec.subtitle].compactMap { $0 }.joined(separator: ", ")
    }

    // MARK: Icons

    private static var icons: [MenuIcon: NSImage] = [:]

    /// The icon's image, made once per distinct icon. Ring figures are rounded to whole percent so the cache stays
    /// small, and each image draws its colours when drawn, so it follows light, dark and Increase Contrast.
    static func image(_ icon: MenuIcon) -> NSImage? {
        let key: MenuIcon = if case .ring(let fraction?, let tint) = icon { .ring((fraction * 100).rounded() / 100, tint) } else { icon }
        if let cached = icons[key] { return cached }
        let image: NSImage?
        switch key {
        case .symbol(let name):
            image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 13, weight: .regular))
            image?.isTemplate = true
        case .ring(let fraction, let tint):
            image = ring(fraction, tint, mark: nil)
        case .mark(let name, let tint):
            image = ring(1, tint, mark: name)
        }
        if icons.count > 256 { icons.removeAll() }
        icons[key] = image
        return image
    }

    private static func ring(_ fraction: Double?, _ tint: MenuTint, mark: String?) -> NSImage {
        // Made once with the tint as a dynamic palette colour, so drawing allocates no image.
        let glyph = mark.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: nil) }?
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 7.5, weight: .heavy)
                .applying(.init(paletteColors: [tint.color])))
        let size = NSSize(width: iconSize, height: iconSize)
        return NSImage(size: size, flipped: false) { rect in
            let color = tint.color, line: CGFloat = 2
            let bounds = rect.insetBy(dx: 1 + line / 2, dy: 1 + line / 2)
            let center = NSPoint(x: bounds.midX, y: bounds.midY), radius = bounds.width / 2
            let track = NSBezierPath(ovalIn: bounds)
            track.lineWidth = line
            color.withAlphaComponent(mark == nil ? 0.25 : 1).setStroke()
            track.stroke()
            if mark == nil, let fraction, fraction > 0 {
                let arc = NSBezierPath()
                arc.appendArc(withCenter: center, radius: radius, startAngle: 90,
                              endAngle: 90 - 360 * CGFloat(min(fraction, 1)), clockwise: true)
                arc.lineWidth = line
                arc.lineCapStyle = .round
                color.setStroke()
                arc.stroke()
            }
            if let glyph {
                glyph.draw(in: NSRect(x: center.x - glyph.size.width / 2, y: center.y - glyph.size.height / 2,
                                       width: glyph.size.width, height: glyph.size.height))
            }
            return true
        }
    }
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

// MARK: Information views

/// A row that shows information and is never chosen: the title row and an account's details. Its menu item is
/// disabled, so keyboard navigation passes over it, and it reads as one static text to VoiceOver.
@MainActor
class MenuInfoView: NSView {
    static let width: CGFloat = 280
    override var isFlipped: Bool { true }
    override var allowsVibrancy: Bool { true }

    init(height: CGFloat) {
        super.init(frame: NSRect(x: 0, y: 0, width: Self.width, height: height))
        autoresizingMask = .width
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    /// The view for an entry, reusing `view` when it is already the right kind; nil for entries that are items.
    static func make(for entry: MenuEntry, reusing view: NSView?) -> MenuInfoView? {
        switch entry {
        case .title(let title, let trailing):
            let row = view as? TitleRowView ?? TitleRowView()
            row.set(title, trailing)
            return row
        case .window(let window):
            let row = view as? WindowRowView ?? WindowRowView()
            row.set(window)
            return row
        case .fact(let key, let value):
            let row = view as? FactRowView ?? FactRowView()
            row.set(key, value)
            return row
        case .note(let text, let tint):
            let row = view as? NoteRowView ?? NoteRowView()
            row.set(text, tint)
            return row
        case .header, .item, .separator:
            return nil
        }
    }

    /// Whether `view` is the kind of view `entry` draws, so it can be updated in place.
    static func matches(_ entry: MenuEntry, _ view: NSView?) -> Bool {
        switch entry {
        case .title: view is TitleRowView
        case .window: view is WindowRowView
        case .fact: view is FactRowView
        case .note: view is NoteRowView
        case .header, .item, .separator: false
        }
    }

    /// Draws one line of text in `rect`, truncated at the end if it does not fit.
    func draw(_ text: NSAttributedString, in rect: NSRect) {
        text.draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    static func text(_ string: String, _ font: NSFont, _ color: NSColor, alignment: NSTextAlignment = .left,
                     truncating: NSLineBreakMode = .byTruncatingTail) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = alignment
        paragraph.lineBreakMode = truncating
        return NSAttributedString(string: string, attributes: [.font: font, .foregroundColor: color, .paragraphStyle: paragraph])
    }
}

/// The menu's first row: its name in bold, and how fresh the data is at the trailing edge.
final class TitleRowView: MenuInfoView {
    private var title = NSAttributedString(), trailing: NSAttributedString?

    init() { super.init(height: 28) }

    func set(_ title: String, _ trailing: String?) {
        self.title = Self.text(title, .boldSystemFont(ofSize: NSFont.systemFontSize), .labelColor)
        self.trailing = trailing.map { Self.text($0, MenuStyle.subtitleFont, .secondaryLabelColor, alignment: .right) }
        setAccessibilityLabel([title, trailing].compactMap { $0 }.joined(separator: ", "))
        needsDisplay = true
    }

    override func draw(_: NSRect) {
        let inset = MenuStyle.inset, line = bounds.insetBy(dx: inset, dy: 0)
        draw(title, in: NSRect(x: inset, y: 6, width: line.width * 0.5, height: 17))
        if let trailing { draw(trailing, in: NSRect(x: line.midX, y: 7.5, width: line.width / 2, height: 16)) }
    }
}

/// A usage window: name and figure on one line, a bar for the use, and when it resets beneath.
final class WindowRowView: MenuInfoView {
    private var title = NSAttributedString(), value = NSAttributedString(), caption: NSAttributedString?
    private var fraction: Double?, tint = MenuTint.secondary

    init() { super.init(height: 50) }

    func set(_ window: WindowSpec) {
        title = Self.text(window.title, MenuStyle.titleFont, .labelColor)
        value = Self.text(window.value, MenuStyle.figureFont, .labelColor, alignment: .right)
        caption = window.caption.map { Self.text($0, MenuStyle.captionFont, .secondaryLabelColor) }
        (fraction, tint) = (window.fraction, window.tint)
        setAccessibilityLabel([window.title, window.value + " used", window.caption].compactMap { $0 }.joined(separator: ", "))
        needsDisplay = true
    }

    override func draw(_: NSRect) {
        let inset = MenuStyle.inset, width = bounds.width - 2 * inset
        let figure = MenuStyle.width(value)
        draw(title, in: NSRect(x: inset, y: 5, width: width - figure - 12, height: 17))
        draw(value, in: NSRect(x: bounds.width - inset - figure - 1, y: 5, width: figure + 1, height: 17))
        let bar = NSRect(x: inset, y: 26, width: width, height: 4)
        NSColor.quaternaryLabelColor.setFill()
        NSBezierPath(roundedRect: bar, xRadius: 2, yRadius: 2).fill()
        if let fraction, fraction > 0 {
            var fill = bar
            fill.size.width = max(bar.height, bar.width * fraction)
            tint.color.setFill()
            NSBezierPath(roundedRect: fill, xRadius: 2, yRadius: 2).fill()
        }
        if let caption { draw(caption, in: NSRect(x: inset, y: 33, width: width, height: 15)) }
    }
}

/// A labelled fact: the label at the leading edge and its value at the trailing edge.
final class FactRowView: MenuInfoView {
    private var key = NSAttributedString(), value = NSAttributedString()

    init() { super.init(height: 22) }

    func set(_ key: String, _ value: String) {
        self.key = Self.text(key, MenuStyle.titleFont, .secondaryLabelColor)
        self.value = Self.text(value, MenuStyle.titleFont, .labelColor, alignment: .right, truncating: .byTruncatingMiddle)
        setAccessibilityLabel("\(key), \(value)")
        needsDisplay = true
    }

    override func draw(_: NSRect) {
        let inset = MenuStyle.inset, width = bounds.width - 2 * inset
        let label = min(MenuStyle.width(key), width / 2)
        draw(key, in: NSRect(x: inset, y: 3, width: label, height: 17))
        draw(value, in: NSRect(x: inset + label + 12, y: 3, width: width - label - 12, height: 17))
    }
}

/// A line of detail, tinted when it reports a problem.
final class NoteRowView: MenuInfoView {
    private var text = NSAttributedString()

    init() { super.init(height: 22) }

    func set(_ text: String, _ tint: MenuTint?) {
        self.text = Self.text(text, MenuStyle.subtitleFont, tint?.color ?? .secondaryLabelColor)
        setAccessibilityLabel(text)
        needsDisplay = true
    }

    override func draw(_: NSRect) {
        draw(text, in: NSRect(x: MenuStyle.inset, y: 4, width: bounds.width - 2 * MenuStyle.inset, height: 16))
    }
}
