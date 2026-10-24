import AppKit
import SwiftUI

/// `quota-menubar --render <report.json> <out.png> [--dark] [--expand <id>,...] [--screen-height <points>]
/// [--now <ISO time>] [--scale <factor>]` draws the panel from a saved report into a PNG, offscreen, without running
/// the CLI, at 2x as on a Retina display unless told otherwise.
/// The clock defaults to the report's `generated_unix_seconds`, so a fixture draws the same way on any day.
@MainActor
enum Render {
    static func run(_ arguments: [String]) -> Int32 {
        var positional: [String] = [], dark = false, expand: Set<String> = [], screen: CGFloat = 900, now: Date?, scale = 2
        var rest = arguments[...]
        while let argument = rest.popFirst() {
            switch argument {
            case "--dark": dark = true
            case "--expand": expand = Set((rest.popFirst() ?? "").split(separator: ",").map(String.init))
            case "--screen-height": screen = rest.popFirst().flatMap(Double.init).map { CGFloat($0) } ?? screen
            case "--now": now = rest.popFirst().flatMap { try? Date($0, strategy: .iso8601) }
            case "--scale": scale = rest.popFirst().flatMap(Int.init).map { max(1, min($0, 4)) } ?? scale
            default: positional.append(argument)
            }
        }
        guard positional.count == 2 else {
            FileHandle.standardError.write(Data("usage: quota-menubar --render <report.json> <out.png> [--dark] [--expand ids] [--screen-height N] [--now ISO] [--scale N]\n".utf8))
            return 2
        }
        let report: QuotaReport
        do {
            let data = try Data(contentsOf: URL(fileURLWithPath: positional[0]))
            report = try CLI.decode(data).get()
        } catch {
            FileHandle.standardError.write(Data("cannot read \(positional[0]): \(error)\n".utf8))
            return 1
        }
        let clock = now ?? report.generatedAt ?? .now
        let model = QuotaModel(fixture: report, updatedAt: clock)

        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        let panel = QuotaPanel(model: model, fixedNow: clock, expanded: expand, screenHeight: screen)
            .background(Color(nsColor: .windowBackgroundColor))
        let host = NSHostingView(rootView: panel)
        host.appearance = appearance
        let window = NSWindow(contentRect: NSRect(x: -10_000, y: -10_000, width: 300, height: 400), styleMask: .borderless,
                              backing: .buffered, defer: false)
        window.appearance = appearance
        window.contentView = host
        // Geometry readers settle over a few passes: content height, then the cap from the measured chrome.
        for _ in 0..<6 {
            host.layoutSubtreeIfNeeded()
            window.setContentSize(host.fittingSize)
            RunLoop.main.run(until: .now + 0.1)
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
        print("\(positional[1]) \(Int(host.bounds.width))x\(Int(host.bounds.height))")
        return 0
    }
}
