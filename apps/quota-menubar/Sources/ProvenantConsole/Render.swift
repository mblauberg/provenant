import AppKit
import ProvenantKit
import SwiftUI

/// `provenant-console --render <screen> <out.png> --fixtures <dir> [--dark] [--now ISO] [--size WxH] [--scale N]`
/// draws a screen offscreen from recorded CLI output, without running the CLI. `screen` is `overview`, `inbox`,
/// `runs`, `tree`, `detail` (the attempt inspector on its own: an offscreen window cannot draw the live inspector pane), `quota`, `chairs` or `search`. The fixtures
/// directory holds the recordings in `Tests/ProvenantTestSupport/Fixtures`; the clock defaults to a fixed day so a
/// fixture draws the same way on any day.
@MainActor
enum Render {
    static func start(_ arguments: [String]) {
        var positional: [String] = [], dark = false, fixtures: String?, now = "2026-11-02T09:00:00Z"
        var size = NSSize(width: 1_180, height: 720), scale = 2
        var rest = arguments[...]
        while let argument = rest.popFirst() {
            switch argument {
            case "--dark": dark = true
            case "--fixtures": fixtures = rest.popFirst()
            case "--now": now = rest.popFirst() ?? now
            case "--scale": scale = rest.popFirst().flatMap(Int.init).map { max(1, min($0, 4)) } ?? scale
            case "--size":
                let parts = (rest.popFirst() ?? "").split(separator: "x").compactMap { Double($0) }
                if parts.count == 2 { size = NSSize(width: parts[0], height: parts[1]) }
            default: positional.append(argument)
            }
        }
        guard positional.count == 2, let fixtures, let date = try? Date(now, strategy: .iso8601) else {
            FileHandle.standardError.write(Data("usage: provenant-console --render <screen> <out.png> --fixtures <dir> [--dark] [--now ISO] [--size WxH] [--scale N]\n".utf8))
            exit(2)
        }
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let job = Job(screen: positional[0], output: positional[1], fixtures: URL(fileURLWithPath: fixtures), dark: dark,
                      now: date, size: size, scale: scale)
        Task { exit(await job.run()) }
        app.run()
    }

    @MainActor struct Job {
        let screen: String, output: String, fixtures: URL, dark: Bool, now: Date, size: NSSize, scale: Int

        /// The recording for a command: `console board` reads `console-board.json`, `inbox` reads `inbox-console.json`.
        func client() -> CLIClient {
            CLIClient { arguments, _ in
                let name: String
                switch arguments.first {
                case "console", "quota": name = arguments.prefix(2).joined(separator: "-")
                case "inbox": name = "inbox-console"
                default: name = arguments.first ?? ""
                }
                let isOutput = arguments.first == "output"
                let base = (isOutput ? "output-\(arguments.contains("prompt") ? "prompt" : "result")" : name) + ".json"
                // A richer recording for drawing may sit in `render/`; otherwise the contract recording is used.
                let file = [fixtures.appending(path: "render/\(base)"), fixtures.appending(path: base)]
                    .first { FileManager.default.fileExists(atPath: $0.path) } ?? fixtures.appending(path: base)
                guard let data = try? Data(contentsOf: file) else { throw ClientError.launch("no recording for \(arguments.joined(separator: " "))") }
                return CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false, out: data, err: "")
            }
        }

        func run() async -> Int32 {
            let console = ConsoleModel(cli: client(), now: { now })
            for destination in Destination.numbered { await console.load(destination) }
            switch screen {
            case "overview": console.select(.overview)
            case "inbox": console.select(.inbox(.all))
            case "runs": console.select(.runs)
            case "tree":
                console.select(.runs)
                await console.runs.setMode(.tree)
            case "detail":
                console.select(.runs)
                if let first = console.runs.board.value?.lanes.first {
                    console.runs.selection = first.id
                    await console.attempt.show(first.id)
                    return await draw(AttemptInspector(console: console).frame(width: 420, height: size.height - 100)
                        .background(.background), size: NSSize(width: 420, height: size.height - 100))
                }
            case "quota": console.select(.quota)
            case "chairs": console.select(.chairs)
            case "search":
                console.searchScope = .results
                console.searchText = "fixture"
                console.select(.search)
                await console.search.run("fixture", part: .result)
            default:
                FileHandle.standardError.write(Data("unknown screen \(screen)\n".utf8))
                return 2
            }
            return await draw(ConsoleView(console: console), size: size)
        }

        private func settle() {
            for _ in 0..<8 { RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1)) }
        }

        private func draw<Root: View>(_ root: Root, size: NSSize) async -> Int32 {
            let appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            NSApp.appearance = appearance
            let window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled, .closable, .resizable],
                                  backing: .buffered, defer: false)
            window.appearance = appearance
            window.contentView = NSHostingView(rootView: root)
            window.setContentSize(size)
            window.setFrameOrigin(NSPoint(x: 40, y: 60))
            window.makeKeyAndOrderFront(nil)
            // Let SwiftUI lay out and the lists populate before the snapshot.
            settle()
            window.setContentSize(size)
            settle()
            FileHandle.standardError.write(Data("window \(window.frame) content \(window.contentView?.frame ?? .zero)\n".utf8))
            guard let view = window.contentView?.superview ?? window.contentView,
                  let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return 1 }
            appearance?.performAsCurrentDrawingAppearance { view.cacheDisplay(in: view.bounds, to: bitmap) }
            guard let png = bitmap.representation(using: .png, properties: [:]) else { return 1 }
            do { try png.write(to: URL(fileURLWithPath: output)) } catch {
                FileHandle.standardError.write(Data("cannot write \(output): \(error)\n".utf8))
                return 1
            }
            print("\(output) \(Int(view.bounds.width))x\(Int(view.bounds.height))")
            return 0
        }
    }
}
