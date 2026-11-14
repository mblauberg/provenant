import Foundation

/// Where the CLI keeps its state: `$PROVENANT_STATE_DIR` when absolute, else `~/.local/share/provenant/v2`.
public enum StateDirectory {
    public static func resolve(environment: [String: String] = ProcessInfo.processInfo.environment,
                               home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        if let dir = environment["PROVENANT_STATE_DIR"], dir.hasPrefix("/") { return URL(fileURLWithPath: dir) }
        return home.appending(path: ".local/share/provenant/v2")
    }

    /// The owner's change signal file inside a state directory.
    public static func signalFile(in state: URL) -> URL { state.appending(path: "changes.json") }
}

/// Wakes a reader when the owner's change signal moves, without a timer. The signal file is only a hint: an event
/// asks `console changes` what moved against the last generations seen, and `onChange` hears the answer. The first
/// answer has no baseline, so every scope reads as unknown and is flagged `isBaseline`; a caller that has just read
/// everything may ignore it. Later answers with unknown (nil) scopes mean reload. Events that arrive during a
/// read fold into one more read after it. Dispatch sources live here, in the shared library, so the console target
/// stays free of them.
@MainActor
public final class ChangeWatcher {
    public typealias Read = @Sendable (_ since: Generations?) async throws -> Changes
    public typealias Opener = (String) -> Int32

    private let signal: URL, directory: URL
    private let read: Read, opener: Opener
    private let onChange: (ChangedScopes, _ isBaseline: Bool) -> Void
    private var sources: [String: DispatchSourceFileSystemObject] = [:]
    private var baseline: Generations?
    private var reading = false, pending = false, stopped = true

    public init(stateDirectory: URL = StateDirectory.resolve(), read: @escaping Read,
                opener: @escaping Opener = { open($0, O_EVTONLY) },
                onChange: @escaping (ChangedScopes, _ isBaseline: Bool) -> Void) {
        directory = stateDirectory.standardized
        signal = StateDirectory.signalFile(in: directory)
        self.read = read
        self.opener = opener
        self.onChange = onChange
    }

    public convenience init(stateDirectory: URL = StateDirectory.resolve(), client: ConsoleClient,
                            onChange: @escaping (ChangedScopes, _ isBaseline: Bool) -> Void) {
        self.init(stateDirectory: stateDirectory, read: { try await client.changes(since: $0) }, onChange: onChange)
    }

    /// Starts watching and reads once, so the first answer reloads everything.
    public func start() {
        guard stopped else { return }
        stopped = false
        rewatch()
        eventOccurred()
    }

    public func stop() {
        stopped = true
        sources.values.forEach { $0.cancel() }
        sources.removeAll()
    }

    /// An owner write, or any other reason to ask again. Reads one at a time.
    public func eventOccurred() {
        guard !stopped else { return }
        guard !reading else { pending = true; return }
        reading = true
        Task { [read, baseline] in
            let result = try? await read(baseline)
            finish(result)
        }
    }

    private func finish(_ result: Changes?) {
        reading = false
        if let result {
            let first = baseline == nil
            baseline = result.generations
            onChange(result.changed, first)
        }
        if pending {
            pending = false
            eventOccurred()
        }
    }

    /// Watches the signal file, and the nearest existing directory above it to hear the file appear or be replaced.
    private func rewatch() {
        var folder = directory
        while !FileManager.default.fileExists(atPath: folder.path), folder.path != "/" { folder.deleteLastPathComponent() }
        var wanted = [folder.path]
        if FileManager.default.fileExists(atPath: signal.path) { wanted.append(signal.path) }
        for path in Array(sources.keys) where !wanted.contains(path) { sources.removeValue(forKey: path)?.cancel() }
        for path in wanted where sources[path] == nil {
            let fd = opener(path)
            guard fd >= 0 else { continue }
            let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .extend, .delete, .rename],
                                                                    queue: .main)
            source.setEventHandler { [weak self, weak source] in
                MainActor.assumeIsolated {
                    guard let self, let source else { return }
                    if !source.data.isDisjoint(with: [.delete, .rename]) { self.sources.removeValue(forKey: path)?.cancel() }
                    self.rewatch()
                    self.eventOccurred()
                }
            }
            source.setCancelHandler { close(fd) }
            sources[path] = source
            source.resume()
        }
    }
}
