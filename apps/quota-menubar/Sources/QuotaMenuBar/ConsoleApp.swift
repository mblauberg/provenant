import AppKit

/// Opens the Provenant Console. The menu agent lives inside the console app, at
/// `Provenant Console.app/Contents/Library/LoginItems/<agent>.app`, so the console is the nearest enclosing `.app`.
/// A bundle identifier lookup is the fallback for an agent run from elsewhere, such as a development build.
enum ConsoleApp {
    static let bundleIdentifier = "dev.provenant.console"

    /// The first `.app` above `bundle` whose identifier is the console's, or nil when the agent is not nested in one.
    static func enclosingApp(of bundle: URL, identifier: (URL) -> String? = { Bundle(url: $0)?.bundleIdentifier }) -> URL? {
        var url = bundle.standardizedFileURL.deletingLastPathComponent()
        while url.path != "/" {
            if url.pathExtension == "app", identifier(url) == bundleIdentifier { return url }
            url.deleteLastPathComponent()
        }
        return nil
    }

    static func open(bundle: URL = Bundle.main.bundleURL, workspace: NSWorkspace = .shared) {
        guard let url = enclosingApp(of: bundle) ?? workspace.urlForApplication(withBundleIdentifier: bundleIdentifier) else {
            log.error("Open Console: no app found for \(bundleIdentifier, privacy: .public)")
            return
        }
        workspace.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration()) { _, error in
            if let error { log.error("Open Console failed: \(error.localizedDescription, privacy: .public)") }
        }
    }
}
