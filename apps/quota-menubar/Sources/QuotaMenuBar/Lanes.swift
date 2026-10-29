import Foundation

enum LaneBoard {
    /// `$PROVENANT_STATE_DIR`, else the default root. A loaded quota report names its own, two levels up.
    static func stateRoot(report: QuotaReport?, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let path = report?.reportPath { return URL(fileURLWithPath: path).deletingLastPathComponent().deletingLastPathComponent() }
        if let dir = environment["PROVENANT_STATE_DIR"], dir.hasPrefix("/") { return URL(fileURLWithPath: dir) }
        return FileManager.default.homeDirectoryForCurrentUser.appending(path: ".local/share/provenant/v1")
    }
}
