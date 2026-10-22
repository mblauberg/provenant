import AppKit
import IOKit.ps
import Network
import notify
import os

let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "dev.provenant.quota-menubar", category: "refresh")

/// What asked for a refresh, as `provenant quota tick --trigger` names it.
enum Trigger: String { case timer, panel, manual, wake, power }

/// Watches report.json and runs `provenant quota tick` only when an account is due or something happens, at most
/// one run at a time. A CLI without `tick` gets the original fixed loop of `provenant quota --json`.
@MainActor @Observable
final class QuotaModel {
    private(set) var report: QuotaReport?
    private(set) var error: String?
    private(set) var updatedAt: Date?
    private(set) var isRefreshing = false
    private(set) var minute = Date.now   // so the label sees a passed reset or exhaustion between refreshes
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var clock: Timer?
    @ObservationIgnored private var launcher: CLI.Launcher?   // resolved once, forgotten if it stops working
    @ObservationIgnored private var ticks: Bool?   // nil until the CLI shows whether it has `quota tick`
    @ObservationIgnored private var pending: Trigger?
    @ObservationIgnored private var watcher: ReportWatcher?
    @ObservationIgnored private var power = Power.current
    @ObservationIgnored private var powerToken: Int32 = 0
    @ObservationIgnored private let network = NWPathMonitor()
    @ObservationIgnored private var online = false
    @ObservationIgnored private var waking = false   // woke, waiting for the network
    @ObservationIgnored private let interval = max(
        ProcessInfo.processInfo.environment["QUOTA_INTERVAL_SECONDS"].flatMap(Double.init) ?? 120, 5)

    init() {
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.timer?.invalidate() }
        }
        center.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.woke() }
        }
        NotificationCenter.default.addObserver(forName: .NSProcessInfoPowerStateDidChange, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.powerChanged() }
        }
        notify_register_dispatch(kIOPSNotifyPowerSource, &powerToken, .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.powerChanged() }
        }
        network.pathUpdateHandler = { [weak self] path in
            MainActor.assumeIsolated { self?.online = path.status == .satisfied; self?.tickOnceOnline() }
        }
        network.start(queue: .main)
        clock = Timer(timeInterval: 60, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.minute = .now } }
        clock?.tolerance = 10
        RunLoop.main.add(clock!, forMode: .common)
        run(.timer)   // also learns whether the CLI has `tick`
    }

    /// ⌘R. Without `tick`, `--refresh` still bypasses the CLI's cache.
    func refresh() { run(.manual) }

    /// The panel can become key twice as it opens; the CLI decides whether data older than that is due.
    /// Without `tick`, the original rule: refresh data older than half an interval.
    func panelOpened() {
        if let updatedAt, Date.now.timeIntervalSince(updatedAt) < (ticks == false ? interval / 2 : 5) { return }
        run(.panel)
    }

    /// A wake always runs `tick`, so a CLI upgraded while asleep is found.
    private func woke() {
        waking = true
        tickOnceOnline()
    }

    /// After wake, once the network is reachable, with up to 15 s of jitter so hosts do not fetch in step.
    private func tickOnceOnline() {
        guard waking, online else { return }
        waking = false
        DispatchQueue.main.asyncAfter(deadline: .now() + .random(in: 0...15)) { [weak self] in
            MainActor.assumeIsolated { self?.run(.wake) }
        }
    }

    private func powerChanged() {
        let now = Power.current
        guard now != power else { return }
        power = now
        if ticks == true { run(.power) }
    }

    private func run(_ trigger: Trigger) {
        guard !isRefreshing else {
            // The run in flight covers timer and panel; the others still happen after it, the strongest first.
            let strength: [Trigger] = [.power, .wake, .manual]
            if let rank = strength.firstIndex(of: trigger), rank > pending.flatMap(strength.firstIndex) ?? -1 { pending = trigger }
            return
        }
        isRefreshing = true
        let ticking = ticks != false || trigger == .wake
        let arguments = ticking ? ["quota", "tick", "--trigger", trigger.rawValue, "--power", power, "--json"]
            : ["quota", "--json"] + (trigger == .manual ? ["--refresh"] : [])
        if ticking { log.info("tick: trigger \(trigger.rawValue, privacy: .public), power \(self.power, privacy: .public)") }
        let cached = launcher
        Task {
            let (resolved, outcome) = await withCheckedContinuation { done in
                DispatchQueue.global(qos: .utility).async { done.resume(returning: CLI.quota(cached, arguments)) }
            }
            launcher = resolved
            isRefreshing = false
            switch outcome {
            case .report(let report):
                // Only a run of `tick` shows the CLI has it.
                if ticking, ticks != true { ticks = true; watch() }
                watcher?.arm()   // the first tick may have created the directory
                show(report)
            case .failure(let failure):
                error = failure.message   // the last good report stays, flagged
                if ticks != false { schedule(nil) }
            case .unsupported:
                log.notice("quota tick is unsupported; polling every \(Int(self.interval)) s")
                (ticks, watcher, pending) = (false, nil, nil)
                return startLoop()
            }
            if let next = pending { pending = nil; run(next) }
        }
    }

    /// Runs and the watcher both deliver reports, in either order; an older one never replaces a newer.
    private func show(_ incoming: QuotaReport) {
        let report = self.report.flatMap { shown in
            shown.generatedAt.flatMap { at in incoming.generatedAt.map { $0 < at } } == true ? shown : nil } ?? incoming
        (self.report, error, updatedAt) = (report, nil, .now)
        if ticks == true { schedule(report.nextDueAt ?? report.nextCheck?.nextDueAt) }
    }

    /// One timer at the earliest due time, but no sooner than 30 s, so a report that is always due cannot spin.
    private func schedule(_ due: Date?) {
        timer?.invalidate()
        let delay = max(due?.timeIntervalSinceNow ?? interval, 30)
        timer = Timer(timeInterval: delay, repeats: false) { [weak self] _ in MainActor.assumeIsolated { self?.run(.timer) } }
        timer?.tolerance = min(delay * 0.1, 60)   // lets the system coalesce wakeups
        RunLoop.main.add(timer!, forMode: .common)
    }

    private func watch() {
        watcher = ReportWatcher(dir: CLI.stateDir) { [weak self] data in
            switch CLI.decode(data) {
            case .success(let report): MainActor.assumeIsolated { self?.show(report) }
            case .failure: log.error("report.json is unreadable")
            }
        }
    }

    /// The original fixed loop, for a CLI without `tick`.
    private func startLoop() {
        timer?.invalidate()
        timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.run(.timer) } }
        timer?.tolerance = interval * 0.2
        RunLoop.main.add(timer!, forMode: .common)
        run(.timer)
    }
}

/// Follows `<state dir>/report.json`. The CLI replaces it by atomic rename, which shows as a write on the
/// directory, so the source watches the directory and re-arms when the directory itself is removed or moved.
final class ReportWatcher {
    private let dir: URL, changed: (Data) -> Void
    private var source: DispatchSourceFileSystemObject?
    private var seen: [Int]?

    init(dir: URL, changed: @escaping (Data) -> Void) {
        (self.dir, self.changed) = (dir, changed)
        arm()
    }

    deinit { source?.cancel() }

    /// Opens the directory unless it is already watched; it may not exist until the first tick creates it.
    func arm() {
        guard source == nil else { return }
        let fd = open(dir.path, O_EVTONLY)
        guard fd >= 0 else { return }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .delete, .rename], queue: .main)
        source.setEventHandler { [weak self] in
            guard let self, let events = self.source?.data else { return }
            if !events.isDisjoint(with: [.delete, .rename]) {
                log.info("state dir moved or removed; re-arming the watcher")
                self.source?.cancel()
                self.source = nil
                self.arm()
            }
            self.check()
        }
        source.setCancelHandler { close(fd) }
        source.resume()
        self.source = source
        log.info("watching the quota state dir")
        check()
    }

    /// Reads the report only when its inode, size or modification time changed; snapshot and lock files share the directory.
    private func check() {
        let path = dir.appending(path: "report.json").path
        var info = stat()
        guard stat(path, &info) == 0 else { return }
        let stamp = [Int(info.st_ino), Int(info.st_size), info.st_mtimespec.tv_sec, info.st_mtimespec.tv_nsec]
        guard stamp != seen, let data = FileManager.default.contents(atPath: path) else { return }
        seen = stamp
        changed(data)
    }
}

/// The `--power` value; Low Power Mode wins over the power source.
enum Power {
    static var current: String {
        if ProcessInfo.processInfo.isLowPowerModeEnabled { return "low" }
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let source = IOPSGetProvidingPowerSourceType(info)?.takeUnretainedValue() else { return "ac" }
        return source as String == kIOPSBatteryPowerValue ? "battery" : "ac"
    }
}

/// Runs `provenant quota` off the main thread. The app never reads credentials itself.
enum CLI {
    struct Failure: Error { let message: String }
    enum Outcome { case report(QuotaReport), failure(Failure), unsupported }
    struct Launcher: Sendable { let url: URL, path: String?, custom: Bool }
    struct Output { let status: Int32, signalled: Bool, timedOut: Bool, out: Data, err: String }

    /// State shared by the reader, watchdog and calling threads.
    final class Shared: @unchecked Sendable {
        private let lock = NSLock()
        private var out = Data(), err = Data(), closed: Set<Int32> = [], timedOut = false
        func append(_ data: Data, toErr: Bool) { lock.withLock { if toErr { err += data } else { out += data } } }
        /// True the first time a pipe reports end of file.
        func close(_ fd: Int32) -> Bool { lock.withLock { closed.insert(fd).inserted } }
        func markTimedOut() { lock.withLock { timedOut = true } }
        var snapshot: (Data, Data, Bool) { lock.withLock { (out, err, timedOut) } }
    }

    /// Where `tick` writes report.json: provider_quota.py `state_root() / 'quota'`. The CLI inherits this
    /// process's environment, so it sees the same AGENT_FABRIC_STATE_ROOT.
    static var stateDir: URL {
        let root = ProcessInfo.processInfo.environment["AGENT_FABRIC_STATE_ROOT"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appending(path: ".local/state/agent-harness/fabric")
        return root.appending(path: "quota", directoryHint: .isDirectory)
    }

    static func quota(_ cached: Launcher?, _ arguments: [String]) -> (Launcher?, Outcome) {
        // Logs carry the cause only; messages can hold paths and CLI output.
        func fail(_ launcher: Launcher?, _ cause: String, _ message: String) -> (Launcher?, Outcome) {
            log.error("\(arguments[1], privacy: .public) failed: \(cause, privacy: .public)")
            return (launcher, .failure(Failure(message: message)))
        }
        guard let launcher = cached ?? resolve() else {
            return fail(nil, "not found", "provenant is not on the login shell's PATH. Set PROVENANT_BIN.")
        }
        let name = launcher.custom ? "PROVENANT_BIN (\(launcher.url.path))" : "provenant"
        let run: Output
        do { run = try exec(launcher.url, arguments, path: launcher.path, limit: 60) } catch {
            return fail(nil, "launch", "Could not start \(name): \(error.localizedDescription)")
        }
        let detail = run.err.split(separator: "\n").last.map(String.init) ?? "no error output"
        if run.timedOut { return fail(launcher, "timeout", "provenant quota timed out after 60 seconds.") }
        if run.signalled { return fail(launcher, "signal \(run.status)", "provenant quota was killed by signal \(run.status).") }
        if run.status == 127 {   // the launcher or its interpreter is gone: re-resolve next time
            let hint = launcher.custom ? "" : " Check that the login shell's PATH reaches it, or set PROVENANT_BIN."
            return fail(nil, "status 127", "\(name) exited with status 127: \(detail).\(hint)")
        }
        // An older CLI rejects `tick` as a usage error and prints no JSON.
        if run.status != 0, arguments[1] == "tick", (try? JSONSerialization.jsonObject(with: run.out)) == nil {
            return (launcher, .unsupported)
        }
        if run.status != 0 { return fail(launcher, "status \(run.status)", "\(name) exited with status \(run.status): \(detail)") }
        switch decode(run.out) {
        case .success(let report): return (launcher, .report(report))
        case .failure(let failure): return fail(launcher, "unreadable report", failure.message)
        }
    }

    /// PROVENANT_BIN, or `provenant` and PATH from a zsh login shell, so its `env python3` shebang resolves.
    static func resolve() -> Launcher? {
        if let bin = ProcessInfo.processInfo.environment["PROVENANT_BIN"], !bin.isEmpty {
            return Launcher(url: URL(fileURLWithPath: bin), path: nil, custom: true)
        }
        guard let run = try? exec(URL(fileURLWithPath: "/bin/zsh"), ["-lc", "command -v provenant && print -r -- \"$PATH\""],
                                  path: nil, limit: 20), run.status == 0, !run.timedOut else { return nil }
        let lines = String(decoding: run.out, as: UTF8.self).split(separator: "\n").map(String.init)
        guard lines.count >= 2, lines[0].hasPrefix("/") else { return nil }
        return Launcher(url: URL(fileURLWithPath: lines[0]), path: lines[1], custom: false)
    }

    /// Runs the CLI as the leader of its own process group, so an overrun's descendants are signalled with it.
    /// Pipes drain as data arrives, so neither can fill and block the child. At the limit the group gets SIGTERM,
    /// then SIGKILL 3 s later, and a pipe still held open after that is abandoned, so every wait is bounded.
    static func exec(_ url: URL, _ arguments: [String], path: String?, limit: Double) throws -> Output {
        let out = Pipe(), err = Pipe(), shared = Shared(), drained = DispatchGroup(), exited = DispatchSemaphore(value: 0)
        var environment = ProcessInfo.processInfo.environment
        if let path { environment["PATH"] = path }
        var actions: posix_spawn_file_actions_t?, attributes: posix_spawnattr_t?
        posix_spawn_file_actions_init(&actions)
        posix_spawnattr_init(&attributes)
        defer { posix_spawn_file_actions_destroy(&actions); posix_spawnattr_destroy(&attributes) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_adddup2(&actions, out.fileHandleForWriting.fileDescriptor, 1)
        posix_spawn_file_actions_adddup2(&actions, err.fileHandleForWriting.fileDescriptor, 2)
        posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT))
        posix_spawnattr_setpgroup(&attributes, 0)
        let argv = ([url.path] + arguments).map { strdup($0) } + [nil]
        let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer { (argv + envp).forEach { free($0) } }
        var pid: pid_t = 0
        let spawned = posix_spawn(&pid, url.path, &actions, &attributes, argv, envp)
        try? out.fileHandleForWriting.close()   // the child holds its own copies; EOF needs ours closed
        try? err.fileHandleForWriting.close()
        guard spawned == 0 else { throw POSIXError(POSIXErrorCode(rawValue: spawned) ?? .EIO) }
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
        // Waits without reaping: the leader's unreaped pid keeps the group id from being reused while it is signalled.
        DispatchQueue.global(qos: .utility).async {
            var info = siginfo_t()
            while waitid(P_PID, id_t(pid), &info, WEXITED | WNOWAIT) == -1 && errno == EINTR {}
            exited.signal()
        }
        var killed = false
        if exited.wait(timeout: .now() + limit) == .timedOut {
            shared.markTimedOut()
            kill(-pid, SIGTERM)
            let gone = exited.wait(timeout: .now() + 3) == .success
            kill(-pid, SIGKILL)   // also when the leader exited but a descendant ignored SIGTERM
            killed = !gone && exited.wait(timeout: .now() + 3) == .timedOut   // still not exited: leave it unreaped
        }
        if drained.wait(timeout: .now() + 2) == .timedOut {
            kill(-pid, SIGKILL)   // a descendant still holds the pipes
            for pipe in [out, err] { pipe.fileHandleForReading.readabilityHandler = nil }
        }
        var info = siginfo_t()
        if !killed { while waitid(P_PID, id_t(pid), &info, WEXITED) == -1 && errno == EINTR {} }
        let (data, errData, timedOut) = shared.snapshot
        return Output(status: killed ? SIGKILL : info.si_status, signalled: killed || info.si_code != CLD_EXITED,
                      timedOut: timedOut, out: data, err: String(decoding: errData, as: UTF8.self))
    }

    static func decode(_ data: Data) -> Result<QuotaReport, Failure> {
        let report: QuotaReport
        do { report = try QuotaReport.decode(data) } catch {
            let (keys, why): ([CodingKey], String) = switch error as? DecodingError {
            case .keyNotFound(let key, let context): (context.codingPath + [key], "is missing")
            case .typeMismatch(_, let context), .valueNotFound(_, let context), .dataCorrupted(let context):
                (context.codingPath, "is unreadable: \(context.debugDescription)")
            default: ([], "is unreadable: \(error.localizedDescription)")
            }
            let path = keys.map { $0.intValue.map { "[\($0)]" } ?? ".\($0.stringValue)" }.joined().trimmingCharacters(in: ["."])
            return .failure(Failure(message: "The report\(path.isEmpty ? "" : " field \(path)") \(why)"))
        }
        guard report.schema == QuotaReport.schemaID else { return .failure(Failure(message: "Unsupported report schema \(report.schema).")) }
        return .success(report)
    }
}
