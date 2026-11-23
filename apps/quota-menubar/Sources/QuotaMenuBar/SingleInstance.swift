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

    /// Publishes via a temporary file beside `url` and an atomic rename, so a
    /// concurrent reader never observes a partially written marker.
    func write(to url: URL) throws {
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let temporary = directory.appending(
            path: ".\(url.lastPathComponent).\(UUID().uuidString).tmp",
            directoryHint: .notDirectory
        )
        try Data("\(pid)\n\(nonce)".utf8).write(to: temporary)
        defer { try? FileManager.default.removeItem(at: temporary) }
        var failure: Int32 = 0
        let renamed = temporary.path.withCString { source in
            url.path.withCString { destination in
                if rename(source, destination) == 0 { return true }
                failure = errno
                return false
            }
        }
        if !renamed {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(failure), userInfo: nil)
        }
    }
}

enum SingleInstance {
    static let markerURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Caches/dev.provenant.quota-menubar", directoryHint: .isDirectory)
        .appending(path: "instance", directoryHint: .notDirectory)

    /// The executable backing `pid`, or nil when it cannot be inspected.
    static func executablePath(for pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: Int(4 * MAXPATHLEN))
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Whether `pid` still runs this app's executable. A stale marker's PID can
    /// be reused by an unrelated same-user process, which must never be signalled.
    static func runsThisApp(_ pid: pid_t) -> Bool {
        guard let own = executablePath(for: ProcessInfo.processInfo.processIdentifier),
              let other = executablePath(for: pid) else { return false }
        return own == other
    }

    /// The live owner an incoming process must replace, unless it is this
    /// process, is dead, or no longer runs this app's executable.
    static func ownerToReplace(
        _ marker: InstanceMarker?,
        pid: pid_t,
        isAlive: (pid_t) -> Bool,
        isMenuBar: (pid_t) -> Bool
    ) -> pid_t? {
        guard let marker, marker.pid != pid, isAlive(marker.pid), isMenuBar(marker.pid) else { return nil }
        return marker.pid
    }

    /// Stops a verified older owner, publishes this process as the owner, then
    /// confirms after a short delay that no competing launch replaced its marker.
    /// Returns false when this process lost a concurrent-start race and must yield.
    @discardableResult
    static func takeOver(
        url: URL = SingleInstance.markerURL,
        pid: pid_t = ProcessInfo.processInfo.processIdentifier,
        nonce: String = UUID().uuidString,
        terminate: (pid_t) -> Void = { _ = kill($0, SIGTERM) },
        isAlive: (pid_t) -> Bool = { kill($0, 0) == 0 },
        isMenuBar: (pid_t) -> Bool = { SingleInstance.runsThisApp($0) },
        delay: (TimeInterval) -> Void = { Thread.sleep(forTimeInterval: $0) }
    ) -> Bool {
        if let previous = ownerToReplace(
            InstanceMarker(url: url), pid: pid, isAlive: isAlive, isMenuBar: isMenuBar
        ) {
            terminate(previous)
            for _ in 0..<20 where isAlive(previous) { delay(0.05) }
        }
        let marker = InstanceMarker(pid: pid, nonce: nonce)
        do {
            try marker.write(to: url)
        } catch {
            // Without a writable cache we cannot join the handover, but a
            // transient failure must not leave no menu bar after the old owner
            // was stopped.
            return true
        }
        // A competing launch may have overwritten the marker while we published.
        // Re-read once the writes settle: only the process whose nonce survives
        // owns the menu bar.
        delay(0.1)
        return InstanceMarker(url: url) == marker
    }
}
