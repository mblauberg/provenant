import AppKit

/// An account row, drawn by the app in the live menu and in the `--render` preview alike.
///
/// A native item with a submenu reserves about 47 pt for its arrow, and a right tab stop cannot move closer, so the
/// figure column sat 12 pt too far left; a native item also ignores the secondary colour of its attributed title.
/// Drawing the row fixes both. The figure ends `MenuMetrics.arrowColumn` from the right edge and the chevron ends
/// `MenuMetrics.trailing` from it, as in the preview. The highlight follows `enclosingMenuItem.isHighlighted`, so
/// keyboard and mouse tracking, and an open submenu, light the row as for a native item. The menu item still owns
/// the submenu, and the view reads as one menu item to VoiceOver.
@MainActor
final class RowView: NSView {
    private(set) var spec: MenuItemSpec
    private var column: CGFloat
    private var needed: CGFloat
    private let pinned: Bool   // the preview shows the open row with the quiet open-submenu highlight
    override var isFlipped: Bool { true }
    override var allowsVibrancy: Bool { true }
    private static let chevron = NSImage(systemSymbolName: "chevron.right", accessibilityDescription: nil)?
        .withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))

    init(spec: MenuItemSpec, column: CGFloat, open: Bool = false) {
        (self.spec, self.column, pinned) = (spec, column, open)
        needed = Self.width(spec, column)
        super.init(frame: NSRect(x: 0, y: 0, width: needed, height: Self.height(spec)))
        autoresizingMask = .width
        setAccessibilityElement(true)
        setAccessibilityRole(.menuItem)
        setAccessibilityLabel(MenuStyle.spoken(spec))
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    /// The view for a row, reusing `view` when it is already a row.
    static func make(_ spec: MenuItemSpec, column: CGFloat, reusing view: NSView?) -> RowView {
        guard let row = view as? RowView else { return RowView(spec: spec, column: column) }
        row.set(spec, column: column)
        return row
    }

    private func set(_ spec: MenuItemSpec, column: CGFloat) {
        guard spec != self.spec || column != self.column else { return }
        (self.spec, self.column) = (spec, column)
        let width = Self.width(spec, column)
        if width != needed || frame.height != Self.height(spec) {
            needed = width
            setFrameSize(NSSize(width: max(width, frame.width), height: Self.height(spec)))
        }
        setAccessibilityLabel(MenuStyle.spoken(spec))
        needsDisplay = true
    }

    private static func lead(_ spec: MenuItemSpec) -> CGFloat { spec.icon == nil ? MenuStyle.inset : MenuMetrics.iconTitle }
    static func width(_ spec: MenuItemSpec, _ column: CGFloat) -> CGFloat { lead(spec) + column + MenuMetrics.arrowColumn }
    static func height(_ spec: MenuItemSpec) -> CGFloat { spec.subtitle == nil ? MenuMetrics.row : MenuMetrics.tallRow }

    private var highlighted: Bool { enclosingMenuItem?.isHighlighted ?? false }

    override func draw(_: NSRect) {
        let lit = highlighted
        if lit {
            NSColor.selectedContentBackgroundColor.setFill()
            NSBezierPath(roundedRect: bounds.insetBy(dx: 5, dy: 0), xRadius: 8, yRadius: 8).fill()
        } else if pinned {
            NSColor.labelColor.withAlphaComponent(0.08).setFill()
            NSBezierPath(roundedRect: bounds.insetBy(dx: 5, dy: 0), xRadius: 8, yRadius: 8).fill()
        }
        let context = NSGraphicsContext.current?.cgContext
        context?.saveGState()
        if !spec.enabled { context?.setAlpha(0.4) }
        let lead = Self.lead(spec), edge = bounds.width - MenuMetrics.arrowColumn
        let ink: NSColor = lit ? .selectedMenuItemTextColor : .labelColor
        if let image = spec.icon.flatMap(MenuStyle.image) {
            let rect = NSRect(x: MenuMetrics.iconCentre - image.size.width / 2, y: (bounds.height - image.size.height) / 2,
                              width: image.size.width, height: image.size.height)
            if image.isTemplate { Self.tinted(image, ink).draw(in: rect) } else { image.draw(in: rect) }
        }
        let top: CGFloat = spec.subtitle == nil ? 3.5 : 2.5
        var titleEdge = edge
        if spec.value != nil {
            let figure = MenuStyle.figure(spec, highlighted: lit)
            let width = MenuStyle.width(figure)
            figure.draw(with: NSRect(x: edge - width, y: top, width: width, height: 17), options: [.usesLineFragmentOrigin])
            titleEdge = edge - width - MenuStyle.figureGap
        }
        MenuStyle.head(spec, highlighted: lit)
            .draw(with: NSRect(x: lead, y: top, width: max(0, titleEdge - lead), height: 17),
                  options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        if let subtitle = spec.subtitle {
            MenuInfoView.text(subtitle, MenuStyle.subtitleFont, lit ? Self.dimmed : .secondaryLabelColor)
                .draw(with: NSRect(x: lead, y: 18.5, width: max(0, edge - lead), height: 15),
                      options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
        if spec.submenu != nil, let chevron = Self.chevron {
            Self.tinted(chevron, lit ? Self.dimmed : .secondaryLabelColor)
                .draw(in: NSRect(x: bounds.width - MenuMetrics.trailing - chevron.size.width, y: (bounds.height - chevron.size.height) / 2,
                                 width: chevron.size.width, height: chevron.size.height))
        }
        context?.restoreGState()
    }

    static var dimmed: NSColor { NSColor.selectedMenuItemTextColor.withAlphaComponent(0.75) }

    static func tinted(_ image: NSImage, _ color: NSColor) -> NSImage {
        NSImage(size: image.size, flipped: false) { box in
            image.draw(in: box)
            color.set()
            box.fill(using: .sourceAtop)
            return true
        }
    }
}
