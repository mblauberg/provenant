import AppKit
import SwiftUI

/// `quota-menubar --render <report.json> <out.png> [--dark] [--expand <account id>] [--lanes <lanes.json>]
/// [--now <ISO time>] [--scale <factor>]` draws the menu's content from a saved report into a PNG, offscreen,
/// without running the CLI, at 2x as on a Retina display unless told otherwise.
/// AppKit draws a real `NSMenu` only while it tracks the mouse, so the PNG is a preview at standard menu metrics,
/// built from the same `QuotaMenu.entries` as the menu. `--expand` shows that account's submenu beside its row.
/// The clock defaults to the report's `generated_unix_seconds`, so a fixture draws the same way on any day.
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

        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        let host = NSHostingView(rootView: MenuPreview(entries: entries, expand: expand))
        host.appearance = appearance
        let window = NSWindow(contentRect: NSRect(x: -10_000, y: -10_000, width: 300, height: 400), styleMask: .borderless,
                              backing: .buffered, defer: false)
        window.appearance = appearance
        window.contentView = host
        for _ in 0..<3 {
            host.layoutSubtreeIfNeeded()
            window.setContentSize(host.fittingSize)
            RunLoop.main.run(until: .now + 0.05)
        }
        let size = host.bounds.size
        guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width) * scale, pixelsHigh: Int(size.height) * scale,
                                            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return 1 }
        bitmap.size = size
        host.cacheDisplay(in: host.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { return 1 }
        do { try png.write(to: URL(fileURLWithPath: positional[1])) } catch {
            FileHandle.standardError.write(Data("cannot write \(positional[1]): \(error)\n".utf8))
            return 1
        }
        print("\(positional[1]) \(Int(size.width))x\(Int(size.height))")
        return 0
    }
}

/// The menu and, when asked, one account's submenu beside its row, on a desktop-coloured backdrop.
struct MenuPreview: View {
    let entries: [MenuEntry], expand: String?

    var body: some View {
        let submenu = entries.lazy.compactMap { entry -> [MenuEntry]? in
            if case .item(let spec) = entry, spec.id == expand { spec.submenu } else { nil }
        }.first
        HStack(alignment: .row, spacing: -4) {
            MenuSurface(entries: entries, open: expand)
            if let submenu { MenuSurface(entries: submenu, open: nil).alignmentGuide(.row) { $0[.top] + 5 } }
        }
        .padding(12)
        .background(Color(nsColor: .underPageBackgroundColor))
    }
}

extension VerticalAlignment {
    private enum Row: AlignmentID { static func defaultValue(in d: ViewDimensions) -> CGFloat { d[.top] } }
    /// The open row of the menu, which its submenu lines up with.
    static let row = VerticalAlignment(Row.self)
}

/// One menu at standard metrics: 13-point menu font, 22-point rows, section headers, separators, trailing badges,
/// shortcuts and submenu arrows, in system colours.
struct MenuSurface: View {
    let entries: [MenuEntry], open: String?

    var body: some View {
        let images = entries.contains { if case .item(let spec) = $0 { spec.symbol != nil } else { false } }
        VStack(alignment: .leading, spacing: 0) {
            ForEach(entries.indices, id: \.self) { index in
                switch entries[index] {
                case .separator:
                    Divider().padding(.horizontal, 10).padding(.vertical, 5)
                case .header(let title):
                    Text(title).font(.system(size: 11, weight: .semibold)).foregroundStyle(.secondary)
                        .padding(.horizontal, 14).padding(.top, index == 0 ? 2 : 0).padding(.bottom, 2)
                case .item(let spec):
                    if spec.id != nil && spec.id == open {
                        MenuLine(spec: spec, images: images, highlighted: true).alignmentGuide(.row) { $0[.top] }
                    } else {
                        MenuLine(spec: spec, images: images, highlighted: false)
                    }
                }
            }
        }
        .padding(.vertical, 5)
        .frame(minWidth: 220)
        .fixedSize()
        .background(Color(nsColor: .windowBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5))
        .shadow(color: .black.opacity(0.2), radius: 8, y: 4)
    }
}

struct MenuLine: View {
    let spec: MenuItemSpec, images: Bool, highlighted: Bool

    var body: some View {
        let primary: Color = highlighted ? .white : spec.enabled ? .primary : Color(nsColor: .disabledControlTextColor)
        let secondary: Color = highlighted ? .white.opacity(0.8) : spec.enabled ? .secondary : Color(nsColor: .disabledControlTextColor)
        HStack(spacing: 6) {
            if images {
                Group {
                    if let symbol = spec.symbol {
                        Image(systemName: symbol).symbolRenderingMode(.hierarchical)
                            .foregroundStyle(highlighted ? .white : spec.tint.map { Color(nsColor: $0.color) } ?? primary)
                    }
                }
                .frame(width: 16).opacity(spec.enabled ? 1 : 0.6)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(spec.title).foregroundStyle(primary)
                if let subtitle = spec.subtitle { Text(subtitle).font(.system(size: 11)).foregroundStyle(secondary) }
            }
            Spacer(minLength: 24)
            if let badge = spec.badge { Text(badge).foregroundStyle(secondary).monospacedDigit() }
            if let shortcut = spec.shortcut { Text("⌘\(shortcut.uppercased())").foregroundStyle(secondary) }
            if spec.submenu != nil { Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(secondary) }
        }
        .font(.system(size: 13))
        .padding(.horizontal, 9).padding(.vertical, 3).frame(minHeight: 22)
        .background(highlighted ? Color(nsColor: .selectedContentBackgroundColor) : .clear, in: RoundedRectangle(cornerRadius: 5))
        .padding(.horizontal, 5)
    }
}
