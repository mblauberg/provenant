import AppKit
import Darwin
import ProvenantKit
import SwiftUI

/// The console process, or `--render` for an offscreen preview (Render.swift). One window, no timers, no menu agent.
///
/// The window is hand-built rather than a SwiftUI scene so that reopening is explicit and testable: the process
/// stays alive when its window closes, and `applicationShouldHandleReopen` (Dock click, `open`, Finder, the menu
/// agent's "Open Console") makes or fronts the window again. Closing the window stops the change watcher, so a
/// console with no window does no work; opening one reads everything afresh.
@main
enum Main {
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        // Installer-only process: no NSApplication, windows or user-agent changes.
        if arguments.first == "--swap-path" {
            guard arguments.count == 3 else {
                FileHandle.standardError.write(Data("Usage: --swap-path OLD NEW\n".utf8))
                exit(2)
            }
            let result = swapPaths(arguments[1], arguments[2])
            if result != 0 { perror("renamex_np") }
            exit(result == 0 ? 0 : 1)
        }
        guard #available(macOS 26, *) else {
            FileHandle.standardError.write(Data("Provenant Console needs macOS 26 or later.\n".utf8))
            exit(1)
        }
        MainActor.assumeIsolated {
            if arguments.first == "--render" { Render.start(Array(arguments.dropFirst())) }
            let app = NSApplication.shared, delegate = AppDelegate()
            app.delegate = delegate
            app.setActivationPolicy(.regular)
            withExtendedLifetime(delegate) { app.run() }
        }
    }

    static func swapPaths(_ first: String, _ second: String) -> Int32 {
        renamex_np(first, second, UInt32(RENAME_SWAP))
    }
}

@MainActor @available(macOS 26, *)
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, NSMenuItemValidation {
    let console: ConsoleModel
    private let watchesChanges: Bool
    private(set) var window: NSWindow?
    private var watcher: ChangeWatcher?

    /// `watchesChanges` is off in tests, which have no state directory to watch.
    init(console: ConsoleModel? = nil, watchesChanges: Bool = true) {
        self.console = console ?? ConsoleModel(defaults: .standard)
        self.watchesChanges = watchesChanges
    }

    func applicationDidFinishLaunching(_: Notification) {
        NSApp.mainMenu = mainMenu()
        showWindow()
    }

    /// Dock click, `open -a`, Finder or Spotlight while running: bring the window back, making it if it was closed.
    func applicationShouldHandleReopen(_: NSApplication, hasVisibleWindows _: Bool) -> Bool {
        showWindow()
        return false
    }

    /// Menu-bar links arrive here whether Launch Services starts the app or forwards a URL to an open process.
    func application(_: NSApplication, open urls: [URL]) {
        for url in urls {
            guard let route = ConsoleDeepLink.parse(url) else { continue }
            showWindow()
            switch route {
            case .request(let id): Task { await console.openRequest(id) }
            case .attempt(let id): Task { await console.openAttempt(id) }
            }
        }
    }

    /// The app coming to the front is the one automatic refresh besides the change watcher. It reads the screen in
    /// front once; the others are marked out of date and read when chosen.
    func applicationDidBecomeActive(_: Notification) {
        if window?.isVisible == true { Task { await console.refresh() } }
    }

    /// Closing the window does not quit: the Dock icon and "Open Console" in the menu bar reopen it, and an answer
    /// still in flight finishes.
    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { false }

    func applicationSupportsSecureRestorableState(_: NSApplication) -> Bool { true }

    /// Gives answers still in flight a second to return, then removes their reply files. Nothing waits longer.
    func applicationWillTerminate(_: Notification) { ReplyFiles.shared.finish(within: .seconds(1)) }

    func windowWillClose(_: Notification) {
        watcher?.stop()
        watcher = nil
    }

    // MARK: Window

    func showWindow() {
        let window = self.window ?? makeWindow()
        self.window = window
        let wasActive = NSApp.isActive
        window.deminiaturize(nil)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        acknowledgeWindow(window)
        guard watchesChanges, watcher == nil else { return }
        let watcher = ChangeWatcher(client: console.client) { [console] changes, isBaseline in
            // The first answer only sets the baseline; the refresh below has just read everything.
            if !isBaseline { console.apply(changes) }
        }
        self.watcher = watcher
        watcher.start()
        // Becoming active reads the screen; a reopen while already active has no such event.
        if wasActive { Task { await console.refresh() } }
    }

    /// `provenant console open` waits for a fresh PID and nonce from this process. Publish them only after AppKit has
    /// presented a visible, unminimized window; a running process or a stale marker alone proves nothing.
    private func acknowledgeWindow(_ window: NSWindow) {
        guard window.isVisible, !window.isMiniaturized else { return }
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Caches/dev.provenant.console", directoryHint: .isDirectory)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try Data("\(ProcessInfo.processInfo.processIdentifier)\n\(UUID().uuidString)".utf8)
                .write(to: directory.appending(path: "window-ready"), options: .atomic)
        } catch {
            FileHandle.standardError.write(Data("Could not acknowledge console window: \(error)\n".utf8))
        }
    }

    private func makeWindow() -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1_180, height: 720),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        // The SwiftUI content starts below the native titlebar and toolbar. Keep the safe area and the
        // scene-to-window bridge explicit so a toolbar update cannot leave content painted under the chrome.
        window.titlebarAppearsTransparent = false
        let hostingView = NSHostingView(rootView: ConsoleView(console: console))
        hostingView.safeAreaRegions = .all
        hostingView.sceneBridgingOptions = [.title, .toolbars]
        window.title = "Provenant Console"
        window.contentMinSize = NSSize(width: ConsoleView.windowMinimum, height: 560)
        window.contentView = hostingView
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        window.delegate = self
        let autosave = "ProvenantConsoleWindow"
        if !window.setFrameUsingName(autosave) { window.center() }
        window.setFrameAutosaveName(autosave)
        return window
    }

    // MARK: Menu commands (each toolbar control has one)

    @objc private func refresh() { Task { await console.refresh() } }
    @objc private func beginSearch() { console.beginSearch() }
    @objc private func toggleInspector() { console.inspectorShown.toggle() }
    @objc private func runsAsList() { Task { await console.runs.setMode(.list) } }
    @objc private func runsAsTree() { Task { await console.runs.setMode(.tree) } }
    @objc private func showDestination(_ sender: NSMenuItem) {
        if let destination = Destination.numbered.indices.contains(sender.tag) ? Destination.numbered[sender.tag] : nil {
            console.show(destination)
        }
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(refresh): return !console.refreshing
        case #selector(showDestination(_:)):
            item.state = Destination.numbered.indices.contains(item.tag) && Destination.numbered[item.tag] == console.destination ? .on : .off
        case #selector(toggleInspector):
            item.title = console.inspectorShown ? "Hide Inspector" : "Show Inspector"
        case #selector(runsAsList):
            item.state = console.runs.mode == .list ? .on : .off
            return console.destination == .runs
        case #selector(runsAsTree):
            item.state = console.runs.mode == .tree ? .on : .off
            return console.destination == .runs
        default: break
        }
        return true
    }

    private func mainMenu() -> NSMenu {
        let main = NSMenu()
        func add(_ title: String, _ items: [NSMenuItem]) {
            let item = NSMenuItem(), menu = NSMenu(title: title)
            items.forEach(menu.addItem)
            item.submenu = menu
            main.addItem(item)
        }
        func item(_ title: String, _ action: Selector?, _ key: String = "",
                  _ modifiers: NSEvent.ModifierFlags = .command, target: AnyObject? = nil, tag: Int = 0) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
            item.keyEquivalentModifierMask = modifiers
            item.target = target
            item.tag = tag
            return item
        }

        add("Provenant Console", [
            item("About Provenant Console", #selector(NSApplication.orderFrontStandardAboutPanel(_:)), target: NSApp),
            .separator(),
            item("Hide Provenant Console", #selector(NSApplication.hide(_:)), "h", target: NSApp),
            item("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option], target: NSApp),
            item("Show All", #selector(NSApplication.unhideAllApplications(_:)), target: NSApp),
            .separator(),
            item("Quit Provenant Console", #selector(NSApplication.terminate(_:)), "q", target: NSApp),
        ])

        add("Edit", [
            item("Undo", Selector(("undo:")), "z"),
            item("Redo", Selector(("redo:")), "z", [.command, .shift]),
            .separator(),
            item("Cut", #selector(NSText.cut(_:)), "x"),
            item("Copy", #selector(NSText.copy(_:)), "c"),
            item("Paste", #selector(NSText.paste(_:)), "v"),
            item("Select All", #selector(NSText.selectAll(_:)), "a"),
        ])

        // ⌘1 to ⌘5 go to the sections; ⌘R is here only, not also on a toolbar button.
        let destinations = Destination.numbered.enumerated().map { index, destination in
            item(destination.title, #selector(showDestination(_:)), String(index + 1), target: self, tag: index)
        }
        add("View", destinations + [
            .separator(),
            item("Runs as List", #selector(runsAsList), target: self),
            item("Runs as Tree", #selector(runsAsTree), target: self),
            .separator(),
            item("Search", #selector(beginSearch), "f", target: self),
            item("Refresh", #selector(refresh), "r", target: self),
            item("Show Inspector", #selector(toggleInspector), "i", [.command, .option], target: self),
            .separator(),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
        ])

        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        windowMenu.addItem(item("Zoom", #selector(NSWindow.performZoom(_:))))
        windowMenu.addItem(.separator())
        windowMenu.addItem(item("Close Window", #selector(NSWindow.performClose(_:)), "w"))
        windowMenu.addItem(item("Bring All to Front", #selector(NSApplication.arrangeInFront(_:)), target: NSApp))
        let windowItem = NSMenuItem()
        windowItem.submenu = windowMenu
        main.addItem(windowItem)
        NSApp.windowsMenu = windowMenu
        return main
    }
}

enum ConsoleDeepLink: Equatable {
    case request(String)
    case attempt(String)

    static func parse(_ url: URL) -> ConsoleDeepLink? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme?.lowercased() == "provenant-console",
              components.user == nil, components.password == nil, components.port == nil,
              components.query == nil, components.fragment == nil,
              let host = components.host?.lowercased() else { return nil }

        let encodedPath = components.percentEncodedPath
        guard encodedPath.first == "/" else { return nil }
        let encodedID = String(encodedPath.dropFirst())
        guard !encodedID.isEmpty, !encodedID.contains("/"),
              let id = encodedID.removingPercentEncoding,
              !id.isEmpty,
              !id.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { return nil }
        switch host {
        case "request": return .request(id)
        case "attempt": return .attempt(id)
        default: return nil
        }
    }
}
