import AppKit

/// The AppKit boundary for the menu model: whether the console app can be found, for the "Open Console" row.
/// Opening it goes through `ConsoleApp.open`, which resolves the same app.
enum MenuBarController {
    static func consoleURL(bundle: URL = Bundle.main.bundleURL, workspace: NSWorkspace = .shared) -> URL? {
        ConsoleApp.enclosingApp(of: bundle)
            ?? workspace.urlForApplication(withBundleIdentifier: ConsoleApp.bundleIdentifier)
    }
}
