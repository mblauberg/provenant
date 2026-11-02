import Foundation

/// Runs the `provenant` CLI. Every read goes through `--json`; nothing here opens a database, a credential or a
/// provider store.
public enum CLI {
    public struct Failure: Error { public let message: String; public init(message: String) { self.message = message } }
    public struct Launcher: Sendable {
        public let url: URL, path: String?, custom: Bool
        public init(url: URL, path: String?, custom: Bool) { self.url = url; self.path = path; self.custom = custom }
    }
    public struct Output: Sendable {
        public let status: Int32, signalled: Bool, timedOut: Bool, truncated: Bool, out: Data, err: String
        public init(status: Int32, signalled: Bool, timedOut: Bool, truncated: Bool, out: Data, err: String) {
            self.status = status; self.signalled = signalled; self.timedOut = timedOut
            self.truncated = truncated; self.out = out; self.err = err
        }
    }

    /// The most output a run keeps per stream; the rest is dropped and flagged, so a runaway child cannot grow memory.
    public static let defaultMaxOutput = 8 << 20

    /// State shared by the reader, watchdog and calling threads.
    final class Shared: @unchecked Sendable {
        private let lock = NSLock()
        private let limit: Int
        private var out = Data(), err = Data(), closed: Set<Int32> = [], timedOut = false, truncated = false
        init(limit: Int) { self.limit = limit }
        func append(_ data: Data, toErr: Bool) {
            lock.withLock {
                let kept = toErr ? err.count : out.count, room = max(0, limit - kept)
                if data.count > room { truncated = true }
                if toErr { err += data.prefix(room) } else { out += data.prefix(room) }
            }
        }
        /// True the first time a pipe reports end of file.
        func close(_ fd: Int32) -> Bool { lock.withLock { closed.insert(fd).inserted } }
        func markTimedOut() { lock.withLock { timedOut = true } }
        var snapshot: (Data, Data, Bool, Bool) { lock.withLock { (out, err, timedOut, truncated) } }
    }


    /// PROVENANT_BIN; else `provenant` and PATH from the user's login shell; else ~/.local/bin/provenant
    /// with this process's PATH.
    public static func resolve() -> Launcher? {
        let environment = ProcessInfo.processInfo.environment
        if let bin = environment["PROVENANT_BIN"], !bin.isEmpty {
            return Launcher(url: URL(fileURLWithPath: bin), path: nil, custom: true)
        }
        if let shell = loginShell(environment), let found = lookup(shell) { return found }
        let local = FileManager.default.homeDirectoryForCurrentUser.appending(path: ".local/bin/provenant")
        return FileManager.default.isExecutableFile(atPath: local.path) ? Launcher(url: local, path: nil, custom: false) : nil
    }

    /// $SHELL, else the account's shell; nil when neither is an absolute executable.
    static func loginShell(_ environment: [String: String]) -> URL? {
        var candidates = [environment["SHELL"]]
        if let entry = getpwuid(getuid()), let shell = entry.pointee.pw_shell { candidates.append(String(cString: shell)) }
        return candidates.compactMap { $0 }.first { $0.hasPrefix("/") && FileManager.default.isExecutableFile(atPath: $0) }
            .map { URL(fileURLWithPath: $0) }
    }

    /// `command -v` works in zsh, bash and fish alike. PATH comes from `printenv`, not the shell's own syntax
    /// (fish prints $PATH space-separated), and is optional. Taking the last absolute line skips login banners.
    static func lookup(_ shell: URL) -> Launcher? {
        guard let run = try? exec(shell, ["-lc", "command -v provenant && printenv PATH"], path: nil, limit: 20),
              run.status == 0, !run.timedOut else { return nil }
        let lines = String(decoding: run.out, as: UTF8.self).split(separator: "\n").map(String.init)
        guard let index = lines.lastIndex(where: { $0.hasPrefix("/") && !$0.contains(":") && $0.hasSuffix("/provenant") })
        else { return nil }
        let path = lines.indices.contains(index + 1) && !lines[index + 1].isEmpty ? lines[index + 1] : nil
        return Launcher(url: URL(fileURLWithPath: lines[index]), path: path, custom: false)
    }

    /// Waits for, and finally reaps, the group leader. One waiter owns this, however late the leader exits: it
    /// reports the exit without reaping (so the unreaped pid keeps the group id from being reused while signalling),
    /// then reaps once `release` is called.
    final class Leader: @unchecked Sendable {
        let pid: pid_t
        private let exited = DispatchSemaphore(value: 0), released = DispatchSemaphore(value: 0)
        private let reaped = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var info = siginfo_t()

        init(pid: pid_t) {
            self.pid = pid
            DispatchQueue.global(qos: .utility).async { [self] in
                var status = siginfo_t()
                while waitid(P_PID, id_t(pid), &status, WEXITED | WNOWAIT) == -1 && errno == EINTR {}
                exited.signal()
                released.wait()
                while waitid(P_PID, id_t(pid), &status, WEXITED) == -1 && errno == EINTR {}
                lock.withLock { info = status }
                reaped.signal()
            }
        }

        /// True if the leader exited within `seconds`.
        func waitForExit(seconds: Double) -> Bool { exited.wait(timeout: .now() + seconds) == .success }
        func signalGroup(_ signal: Int32) { kill(-pid, signal) }

        /// Lets the waiter reap, and returns the exit once it has. Only call when the leader has exited.
        func reap() -> siginfo_t {
            released.signal()
            reaped.wait()
            return lock.withLock { info }
        }

        /// Lets the waiter go without waiting for it, for a leader that never exited.
        func abandon() { released.signal() }
    }

    /// Spawns the CLI as the leader of its own process group with stdin on /dev/null and both output pipes
    /// attached. The parent's copies of the write ends are closed, so EOF can arrive.
    static func spawn(_ url: URL, _ arguments: [String], path: String?, qos: qos_class_t,
                      out: Pipe, err: Pipe) throws -> pid_t {
        var environment = ProcessInfo.processInfo.environment
        if let path { environment["PATH"] = path }
        var actions: posix_spawn_file_actions_t?, attributes: posix_spawnattr_t?
        posix_spawn_file_actions_init(&actions)
        posix_spawnattr_init(&attributes)
        defer { posix_spawn_file_actions_destroy(&actions); posix_spawnattr_destroy(&attributes) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_adddup2(&actions, out.fileHandleForWriting.fileDescriptor, 1)
        posix_spawn_file_actions_adddup2(&actions, err.fileHandleForWriting.fileDescriptor, 2)
        // The calling dispatch thread blocks some signals; the CLI starts with none blocked and default handlers.
        var unblocked = sigset_t(), defaults = sigset_t()
        for signal in [SIGTERM, SIGINT, SIGPIPE] { defaults |= 1 << (signal - 1) }
        posix_spawnattr_setsigmask(&attributes, &unblocked)
        posix_spawnattr_setsigdefault(&attributes, &defaults)
        posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF
                                                     | POSIX_SPAWN_CLOEXEC_DEFAULT))
        posix_spawnattr_setpgroup(&attributes, 0)
        posix_spawnattr_set_qos_class_np(&attributes, qos)   // the CLI never competes with foreground work
        let argv = ([url.path] + arguments).map { strdup($0) } + [nil]
        let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer { (argv + envp).forEach { free($0) } }
        var pid: pid_t = 0
        let spawned = posix_spawn(&pid, url.path, &actions, &attributes, argv, envp)
        try? out.fileHandleForWriting.close()   // the child holds its own copies; EOF needs ours closed
        try? err.fileHandleForWriting.close()
        guard spawned == 0 else { throw POSIXError(POSIXErrorCode(rawValue: spawned) ?? .EIO) }
        return pid
    }

    /// Drains both pipes as data arrives, so neither can fill and block the child. The group is left when each
    /// pipe has reported end of file.
    static func drain(out: Pipe, err: Pipe, into shared: Shared) -> DispatchGroup {
        let drained = DispatchGroup()
        for pipe in [out, err] {
            let toErr = pipe === err
            drained.enter()
            pipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty { return shared.append(data, toErr: toErr) }
                handle.readabilityHandler = nil
                if shared.close(handle.fileDescriptor) { drained.leave() }
            }
        }
        return drained
    }

    /// At the limit the group gets SIGTERM and a full `grace`, then SIGKILL. Returns true when the leader is
    /// still running after that.
    static func terminate(_ leader: Leader, grace: Double) -> Bool {
        leader.signalGroup(SIGTERM)
        let gone = leader.waitForExit(seconds: grace)
        // An early exit still leaves descendants the full grace; this sleeps out the rest without clock arithmetic.
        if gone { _ = DispatchSemaphore(value: 0).wait(timeout: .now() + grace) }
        leader.signalGroup(SIGKILL)
        return !gone && !leader.waitForExit(seconds: 3)
    }

    /// Runs the CLI as the leader of its own process group, so an overrun's descendants are signalled with it.
    /// At the limit the group gets SIGTERM and a full grace period, then SIGKILL; a pipe still held open after
    /// that is abandoned. Every wait here is bounded, and one waiter owns reaping the leader, however late it exits.
    public static func exec(_ url: URL, _ arguments: [String], path: String?, limit: Double, qos: qos_class_t = QOS_CLASS_UTILITY,
                            maxOutput: Int = defaultMaxOutput, grace: Double = 3) throws -> Output {
        let out = Pipe(), err = Pipe(), shared = Shared(limit: maxOutput)
        let pid = try spawn(url, arguments, path: path, qos: qos, out: out, err: err)
        let drained = drain(out: out, err: err, into: shared)
        let leader = Leader(pid: pid)
        var running = false
        if !leader.waitForExit(seconds: limit) {
            shared.markTimedOut()
            running = terminate(leader, grace: grace)
        }
        if drained.wait(timeout: .now() + 2) == .timedOut {
            leader.signalGroup(SIGKILL)   // a descendant still holds the pipes
            for pipe in [out, err] { pipe.fileHandleForReading.readabilityHandler = nil }
        }
        let info: siginfo_t
        if running { leader.abandon(); info = siginfo_t() } else { info = leader.reap() }
        let (data, errData, timedOut, truncated) = shared.snapshot
        return Output(status: running ? SIGKILL : info.si_status, signalled: running || info.si_code != CLD_EXITED,
                      timedOut: timedOut, truncated: truncated, out: data, err: String(decoding: errData, as: UTF8.self))
    }
}
