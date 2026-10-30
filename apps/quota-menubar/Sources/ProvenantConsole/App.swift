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
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private let model = InboxModel()
    private var window: NSWindow?

    func applicationDidFinishLaunching(_: Notification) {
        NSApp.mainMenu = mainMenu()
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1_040, height: 680),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Provenant Console"
        window.contentMinSize = NSSize(width: 860, height: 520)
        window.contentView = NSHostingView(rootView: InboxView(model: model))
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        let autosave = "ProvenantConsoleWindow"
        if !window.setFrameUsingName(autosave) { window.center() }
        window.setFrameAutosaveName(autosave)
        window.makeKeyAndOrderFront(nil)
        self.window = window
        NSApp.activate()
        refresh()
    }

    /// The only automatic refresh: the app coming to the front.
    func applicationDidBecomeActive(_: Notification) { refresh() }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }

    func applicationSupportsSecureRestorableState(_: NSApplication) -> Bool { true }

    /// Gives answers still in flight a second to return, then removes their reply files. Nothing waits longer.
    func applicationWillTerminate(_: Notification) { ReplyFiles.shared.finish(within: .seconds(1)) }

    @objc func refresh() { Task { await model.refresh() } }

    @objc private func showAllRequests() { model.show(.filter(.all)) }
    @objc private func showBlockingRequests() { model.show(.filter(.blocking)) }
    @objc private func showDecisionRequests() { model.show(.filter(.decisions)) }
    @objc private func showQuestionRequests() { model.show(.filter(.questions)) }
    @objc private func showAssumptionRequests() { model.show(.filter(.assumptions)) }
    @objc private func beginSearch() { model.beginSearch() }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(refresh) { return !model.refreshing }
        if menuItem.action == #selector(showAllRequests) { menuItem.state = filterState(.all) }
        if menuItem.action == #selector(showBlockingRequests) { menuItem.state = filterState(.blocking) }
        if menuItem.action == #selector(showDecisionRequests) { menuItem.state = filterState(.decisions) }
        if menuItem.action == #selector(showQuestionRequests) { menuItem.state = filterState(.questions) }
        if menuItem.action == #selector(showAssumptionRequests) { menuItem.state = filterState(.assumptions) }
        return true
    }

    private func filterState(_ filter: InboxModel.Filter) -> NSControl.StateValue {
        model.scope == .filter(filter) ? .on : .off
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
                  _ modifiers: NSEvent.ModifierFlags = .command, target: AnyObject? = nil) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
            item.keyEquivalentModifierMask = modifiers
            item.target = target
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

        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        windowMenu.addItem(item("Zoom", #selector(NSWindow.performZoom(_:))))
        windowMenu.addItem(.separator())
        windowMenu.addItem(item("Close Window", #selector(NSWindow.performClose(_:)), "w"))
        windowMenu.addItem(item("Bring All to Front", #selector(NSApplication.arrangeInFront(_:)), target: NSApp))

        add("View", [
            item("All Requests", #selector(showAllRequests), "1", target: self),
            item("Blocking Requests", #selector(showBlockingRequests), "2", target: self),
            item("Decision Requests", #selector(showDecisionRequests), "3", target: self),
            item("Question Requests", #selector(showQuestionRequests), "4", target: self),
            item("Assumption Requests", #selector(showAssumptionRequests), "5", target: self),
            .separator(),
            item("Search Requests", #selector(beginSearch), "f", target: self),
            item("Refresh", #selector(refresh), "r", target: self),
            .separator(),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
        ])

        let windowItem = NSMenuItem()
        windowItem.submenu = windowMenu
        main.addItem(windowItem)
        NSApp.windowsMenu = windowMenu

        return main
    }
}
