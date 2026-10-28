import AppKit

/// The menu bar app, or `--render <report.json> <out.png>` to draw the menu from a saved report (Render.swift).
@main
enum Main {
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if arguments.first == "--render" { exit(MainActor.assumeIsolated { Render.run(Array(arguments.dropFirst())) }) }
        MainActor.assumeIsolated {
            let app = NSApplication.shared, delegate = AppDelegate()
            app.delegate = delegate
            app.setActivationPolicy(.accessory)   // no Dock icon or app menu
            withExtendedLifetime(delegate) { app.run() }
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var menu: StatusMenu?
    private var model: QuotaModel?
    func applicationDidFinishLaunching(_: Notification) {
        let model = QuotaModel()
        self.model = model
        menu = StatusMenu(model: model)
    }
    func applicationWillTerminate(_: Notification) { model?.stop() }
}

/// The Provenant mark as a template image, so it follows the menu bar's appearance and tint.
enum Brand {
    static let glyph: NSImage = {
        let image = Bundle.module.url(forResource: "ProvenantGlyph", withExtension: "svg")
            .flatMap(NSImage.init(contentsOf:)) ?? NSImage()
        image.isTemplate = true
        return image
    }()

    /// The mark at status item size.
    static let statusGlyph: NSImage = {
        let image = glyph.copy() as? NSImage ?? NSImage()
        image.size = NSSize(width: 16, height: 16)
        image.isTemplate = true
        return image
    }()
}

/// What the status item shows. Redrawing a status item costs real CPU, so the model replaces this only when it
/// differs.
struct MenuLabel: Equatable {
    var symbol = "gauge.with.dots.needle.0percent", text: String? = "–", spoken = "Provider quota"
    var usesBrandGlyph = true

    init() {}

    init(report: QuotaReport?, failed: Bool, now: Date) {
        let featured = report?.featured(now: now), used = featured?.window.usedPercent ?? report?.headline(now: now)
        symbol = failed ? "exclamationmark.triangle" : Self.gauge(used ?? 0)
        usesBrandGlyph = !failed && (used.map { $0 < (report?.alertPercent ?? 90) } ?? true)
        text = report.map { report in
            featured.map { Fmt.percent($0.window.usedPercent ?? 0) }
                ?? (report.iconOnly ? nil : report.headline(now: now).map(Fmt.percent) ?? "–")
        } ?? "–"
        let exhausted = report?.exhausted(now: now).count ?? 0
        let drawing = report?.drawing(now: now).count ?? 0
        let unavailable = report?.visible.filter { !$0.hasReading && !$0.requiresAuth }.count ?? 0
        spoken = ["Provider quota",
                  featured.map { "\($0.account.title), \($0.window.fullTitle) \(Fmt.percent($0.window.usedPercent ?? 0)) used" },
                  exhausted > 0 ? "\(exhausted) exhausted" : nil,
                  drawing > 0 ? "\(drawing) drawing on credits" : nil, unavailable > 0 ? "\(unavailable) unavailable" : nil,
                  failed ? "last refresh failed" : nil].compactMap { $0 }.joined(separator: ", ")
    }
}

extension MenuLabel {
    /// The gauge symbol whose needle is nearest a use percentage.
    static func gauge(_ used: Double) -> String {
        "gauge.with.dots.needle.\(["0", "33", "50", "67", "100"][[20, 45, 60, 100].firstIndex { used < Double($0) } ?? 4])percent"
    }
}
