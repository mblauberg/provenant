import Darwin
import Foundation

/// The menu bar owner, recorded as `<pid>\n<nonce>`. A newer launch replaces the
/// recorded process so an upgraded binary takes over from an older one; a
/// missing, malformed or dead marker is not an owner.
struct InstanceMarker: Equatable {
    let pid: pid_t
    let nonce: String

    init(pid: pid_t, nonce: String) {
        self.pid = pid
        self.nonce = nonce
    }

    init?(url: URL) {
        guard let text = try? String(contentsOf: url, encoding: .utf8),
              let newline = text.firstIndex(of: "\n"),
              let pid = Int32(String(text[..<newline])),
              pid > 0 else { return nil }
        let nonce = String(text[text.index(after: newline)...])
        guard !nonce.isEmpty else { return nil }
        self.pid = pid
        self.nonce = nonce
    }

    func write(to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("\(pid)\n\(nonce)".utf8).write(to: url, options: .atomic)
    }
}

enum SingleInstance {
    static let markerURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Caches/dev.provenant.quota-menubar", directoryHint: .isDirectory)
        .appending(path: "instance", directoryHint: .notDirectory)

    /// The live owner an incoming process must replace, unless it is this process.
    static func ownerToReplace(_ marker: InstanceMarker?, pid: pid_t, isAlive: (pid_t) -> Bool) -> pid_t? {
        guard let marker, marker.pid != pid, isAlive(marker.pid) else { return nil }
        return marker.pid
    }

    /// Stops a live older owner, then publishes this process as the owner.
    static func takeOver(
        url: URL = SingleInstance.markerURL,
        pid: pid_t = ProcessInfo.processInfo.processIdentifier,
        terminate: (pid_t) -> Void = { _ = kill($0, SIGTERM) },
        isAlive: (pid_t) -> Bool = { kill($0, 0) == 0 }
    ) {
        if let previous = ownerToReplace(InstanceMarker(url: url), pid: pid, isAlive: isAlive) {
            terminate(previous)
            for _ in 0..<20 where isAlive(previous) { Thread.sleep(forTimeInterval: 0.05) }
        }
        try? InstanceMarker(pid: pid, nonce: UUID().uuidString).write(to: url)
    }
}
