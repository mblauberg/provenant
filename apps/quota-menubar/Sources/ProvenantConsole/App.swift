import AppKit
import ProvenantKit
import SwiftUI

/// The console process: one inbox window, no timers, no menu agent. It quits when its last window closes.
@main
enum Main {
    static func main() {
        guard #available(macOS 26, *) else {
            FileHandle.standardError.write(Data("Provenant Console needs macOS 26 or later.\n".utf8))
            exit(1)
        }
        MainActor.assumeIsolated {
            let app = NSApplication.shared, delegate = AppDelegate()
            app.delegate = delegate
            app.setActivationPolicy(.regular)
            withExtendedLifetime(delegate) { app.run() }
        }
    }
}

@MainActor @available(macOS 26, *)
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let model = InboxModel()
    private var window: NSWindow?

    func applicationDidFinishLaunching(_: Notification) {
        NSApp.mainMenu = Self.menu()
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 560),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Provenant Inbox"
        window.contentView = NSHostingView(rootView: InboxView(model: model))
        window.isReleasedWhenClosed = false
        window.center()
        window.makeKeyAndOrderFront(nil)
        self.window = window
        NSApp.activate()
        refresh()
    }

    /// The only automatic refresh: the app coming to the front.
    func applicationDidBecomeActive(_: Notification) { refresh() }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }

    /// Gives answers still in flight a second to return, then removes their reply files. Nothing waits longer.
    func applicationWillTerminate(_: Notification) { ReplyFiles.shared.finish(within: .seconds(1)) }

    @objc func refresh() { Task { await model.refresh() } }

    private static func menu() -> NSMenu {
        let main = NSMenu()
        func add(_ title: String, _ items: [NSMenuItem]) {
            let item = NSMenuItem(), menu = NSMenu(title: title)
            items.forEach(menu.addItem)
            item.submenu = menu
            main.addItem(item)
        }
        add("Provenant Console", [NSMenuItem(title: "Quit Provenant Console", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")])
        add("Inbox", [
            NSMenuItem(title: "Refresh", action: #selector(AppDelegate.refresh), keyEquivalent: "r"),
            NSMenuItem(title: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"),
        ])
        return main
    }
}
