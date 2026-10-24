import AppKit
import IOKit.ps
import Network
import notify
import os

let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "dev.provenant.quota-menubar", category: "refresh")

/// What asked for a refresh, as `provenant quota tick --trigger` names it.
enum Trigger: String { case timer, panel, manual, wake, power }

/// Watches the current report and runs `provenant quota tick` when an account is due or something happens.
@MainActor @Observable
final class QuotaModel {
    private(set) var report: QuotaReport?
    private(set) var error: String?
    private(set) var updatedAt: Date?
    private(set) var isRefreshing = false
    /// The status item's content, replaced only when it changes.
    private(set) var label = MenuLabel()
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var clock: Timer?   // one shot, at the next reset that could change the label
    @ObservationIgnored private var launcher: CLI.Launcher?   // resolved once, forgotten if it stops working
    @ObservationIgnored private var pending: Trigger?
    @ObservationIgnored private var watcher: ReportWatcher?
    @ObservationIgnored private var power = Power.current
    @ObservationIgnored private var powerToken: Int32 = 0
    @ObservationIgnored private let network = NWPathMonitor()
    @ObservationIgnored private var online = false
    @ObservationIgnored private var waking = false   // woke, waiting for the network
    @ObservationIgnored private let retryInterval: Double = 120
    @ObservationIgnored private var fixture = false   // a saved report to draw; never runs the CLI

    /// A saved report, for `--render`: no timers, watchers or CLI runs.
    init(fixture report: QuotaReport, updatedAt: Date) {
        (self.report, self.updatedAt, fixture) = (report, updatedAt, true)
        label = MenuLabel(report: report, failed: false, now: updatedAt)
    }

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
        loadCurrent()
    }

    /// ⌘R.
    func refresh() { run(.manual) }

    /// The panel can become key twice as it opens; avoid closely spaced ticks.
    func panelOpened() {
        if let updatedAt, Date.now.timeIntervalSince(updatedAt) < 5 { return }
        run(.panel)
    }

    /// A wake always ticks so the CLI can refresh due accounts.
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
        run(.power)
    }

    private func loadCurrent() {
        guard !fixture else { return }
        isRefreshing = true
        let cached = launcher
        Task {
            let (resolved, outcome) = await withCheckedContinuation { done in
                DispatchQueue.global(qos: .utility).async {
                    done.resume(returning: CLI.quota(cached, ["quota", "show", "--json"]))
                }
            }
            launcher = resolved
            isRefreshing = false
            switch outcome {
            case .report(let report):
                show(report)
                if watcher == nil { watch() } else { watcher?.arm() }
                if report.nextDueAt.map({ $0 <= .now }) ?? true { run(.timer) }
            case .failure(let failure):
                error = failure.message
                relabel()
                schedule(nil)
            }
        }
    }

    private func run(_ trigger: Trigger) {
        guard !fixture else { return }
        guard !isRefreshing else {
            let strength: [Trigger] = [.power, .wake, .manual]
            if let rank = strength.firstIndex(of: trigger), rank > pending.flatMap(strength.firstIndex) ?? -1 { pending = trigger }
            return
        }
        isRefreshing = true
        let arguments = ["quota", "tick", "--trigger", trigger.rawValue, "--power", power, "--json"]
        log.info("tick: trigger \(trigger.rawValue, privacy: .public), power \(self.power, privacy: .public)")
        let cached = launcher
        Task {
            let (resolved, outcome) = await withCheckedContinuation { done in
                DispatchQueue.global(qos: .utility).async { done.resume(returning: CLI.quota(cached, arguments)) }
            }
            launcher = resolved
            isRefreshing = false
            switch outcome {
            case .report(let report):
                show(report)
                if watcher == nil { watch() } else { watcher?.arm() }
            case .failure(let failure):
                error = failure.message
                relabel()
                schedule(nil)
            }
            if let next = pending { pending = nil; run(next) }
        }
    }

    /// Runs and the watcher both deliver reports, in either order; an older one never replaces a newer.
    private func show(_ incoming: QuotaReport) {
        let report = self.report.flatMap { shown in incoming.generatedAt < shown.generatedAt ? shown : nil } ?? incoming
        (self.report, error, updatedAt) = (report, nil, .now)
        relabel()
        schedule(report.nextDueAt)
    }

    /// Recomputes the status item's content, and wakes again only when a reset or return could change it. A
    /// repeating clock redrew the status item every minute for nothing; between refreshes only time passing a
    /// reset changes what it shows.
    private func relabel() {
        let now = Date.now, next = MenuLabel(report: report, failed: error != nil, now: now)
        if next != label { label = next }
        clock?.invalidate()
        let dates = (report?.accounts ?? []).flatMap { account in account.windows.compactMap(\.resetAt) + [account.backAt].compactMap { $0 } }
        guard let due = dates.filter({ $0 > now }).min() else { return }
        clock = Timer(timeInterval: due.timeIntervalSince(now) + 1, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.relabel() }
        }
        clock?.tolerance = 5
        RunLoop.main.add(clock!, forMode: .common)
    }

    /// One timer at the earliest due time, but no sooner than 30 s, so a report that is always due cannot spin.
    private func schedule(_ due: Date?) {
        timer?.invalidate()
        let delay = max(due?.timeIntervalSinceNow ?? retryInterval, 30)
        timer = Timer(timeInterval: delay, repeats: false) { [weak self] _ in MainActor.assumeIsolated { self?.run(.timer) } }
        timer?.tolerance = min(delay * 0.1, 60)   // lets the system coalesce wakeups
        RunLoop.main.add(timer!, forMode: .common)
    }

    private func watch() {
        guard let report else { return }
        watcher = ReportWatcher(file: URL(fileURLWithPath: report.reportPath)) { [weak self] data in
            switch CLI.decode(data) {
            case .success(let report): MainActor.assumeIsolated { self?.show(report) }
            case .failure: log.error("report.json is unreadable")
            }
        }
    }

}

/// Follows `<state dir>/report.json`. The CLI replaces it by atomic rename, which shows as a write on the
/// directory, so the source watches the directory and re-arms when the directory itself is removed or moved.
final class ReportWatcher {
    private let file: URL, changed: (Data) -> Void
    private var source: DispatchSourceFileSystemObject?
    private var seen: [Int]?

    init(file: URL, changed: @escaping (Data) -> Void) {
        (self.file, self.changed) = (file, changed)
        arm()
    }

    deinit { source?.cancel() }

    /// Opens the report directory; it may not exist until the first tick creates it.
    func arm() {
        guard source == nil else { return }
        let fd = open(file.deletingLastPathComponent().path, O_EVTONLY)
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
        log.info("watching the quota report directory")
        check()
    }

    private func check() {
        let path = file.path
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
    enum Outcome { case report(QuotaReport), failure(Failure) }
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
        private var exit = siginfo_t()
        var reaped: siginfo_t { get { lock.withLock { exit } } set { lock.withLock { exit = newValue } } }
    }

    static func quota(_ cached: Launcher?, _ arguments: [String]) -> (Launcher?, Outcome) {
        let operation = arguments.dropFirst().first ?? "tick"
        func fail(_ launcher: Launcher?, _ cause: String, _ message: String) -> (Launcher?, Outcome) {
            log.error("quota \(operation, privacy: .public) failed: \(cause, privacy: .public)")
            return (launcher, .failure(Failure(message: message)))
        }
        guard let launcher = cached ?? resolve() else {
            return fail(nil, "not found", "provenant was not found on the login shell's PATH or at ~/.local/bin/provenant. "
                        + "Set PROVENANT_BIN to its path.")
        }
        let name = launcher.custom ? "PROVENANT_BIN (\(launcher.url.path))" : "provenant"
        let run: Output
        do { run = try exec(launcher.url, arguments, path: launcher.path, limit: 60) } catch {
            return fail(nil, "launch", "Could not start \(name): \(error.localizedDescription)")
        }
        let detail = run.err.split(separator: "\n").last.map(String.init) ?? "no error output"
        if run.timedOut { return fail(launcher, "timeout", "provenant quota timed out after 60 seconds.") }
        if run.signalled { return fail(launcher, "signal \(run.status)", "provenant quota was killed by signal \(run.status).") }
        if run.status == 127 {
            let hint = launcher.custom ? "" : " Check that the login shell's PATH reaches it, or set PROVENANT_BIN."
            return fail(nil, "status 127", "\(name) exited with status 127: \(detail).\(hint)")
        }
        if run.status != 0 { return fail(launcher, "status \(run.status)", "\(name) exited with status \(run.status): \(detail)") }
        switch decodeEnvelope(run.out, command: "quota \(operation)") {
        case .success(let report): return (launcher, .report(report))
        case .failure(let failure): return fail(launcher, "unreadable response", failure.message)
        }
    }

    /// PROVENANT_BIN; else `provenant` and PATH from the user's login shell, so its `env python3` shebang
    /// resolves; else ~/.local/bin/provenant with this process's PATH.
    static func resolve() -> Launcher? {
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

    /// Runs the CLI as the leader of its own process group, so an overrun's descendants are signalled with it.
    /// Pipes drain as data arrives, so neither can fill and block the child. At the limit the group gets SIGTERM
    /// and a full grace period, then SIGKILL; a pipe still held open after that is abandoned. Every wait here is
    /// bounded, and one waiter owns reaping the leader, however late it exits.
    static func exec(_ url: URL, _ arguments: [String], path: String?, limit: Double) throws -> Output {
        let out = Pipe(), err = Pipe(), shared = Shared(), drained = DispatchGroup()
        let exited = DispatchSemaphore(value: 0), released = DispatchSemaphore(value: 0), reaped = DispatchSemaphore(value: 0)
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
        // The one owner of the leader's exit. It reaps only once signalling is over, so until then the unreaped
        // pid keeps the group id from being reused.
        DispatchQueue.global(qos: .utility).async {
            var info = siginfo_t()
            while waitid(P_PID, id_t(pid), &info, WEXITED | WNOWAIT) == -1 && errno == EINTR {}
            exited.signal()
            released.wait()
            while waitid(P_PID, id_t(pid), &info, WEXITED) == -1 && errno == EINTR {}
            shared.reaped = info
            reaped.signal()
        }
        var running = false
        if exited.wait(timeout: .now() + limit) == .timedOut {
            shared.markTimedOut()
            kill(-pid, SIGTERM)
            let grace = DispatchTime.now() + 3
            let gone = exited.wait(timeout: grace) == .success
            if gone { _ = DispatchSemaphore(value: 0).wait(timeout: grace) }   // sleeps out the rest; no clock arithmetic
            kill(-pid, SIGKILL)   // descendants get the full grace even when the leader exits early
            running = !gone && exited.wait(timeout: .now() + 3) == .timedOut
        }
        if drained.wait(timeout: .now() + 2) == .timedOut {
            kill(-pid, SIGKILL)   // a descendant still holds the pipes
            for pipe in [out, err] { pipe.fileHandleForReading.readabilityHandler = nil }
        }
        released.signal()
        if !running { reaped.wait() }   // the leader has exited, so this is immediate
        let (data, errData, timedOut) = shared.snapshot, info = shared.reaped
        return Output(status: running ? SIGKILL : info.si_status, signalled: running || info.si_code != CLD_EXITED,
                      timedOut: timedOut, out: data, err: String(decoding: errData, as: UTF8.self))
    }

    static func decode(_ data: Data) -> Result<QuotaReport, Failure> {
        do { return .success(try QuotaReport.decode(data)) } catch {
            let (keys, why): ([CodingKey], String) = switch error as? DecodingError {
            case .keyNotFound(let key, let context): (context.codingPath + [key], "is missing")
            case .typeMismatch(_, let context), .valueNotFound(_, let context), .dataCorrupted(let context):
                (context.codingPath, "is unreadable: \(context.debugDescription)")
            default: ([], "is unreadable: \(error.localizedDescription)")
            }
            let path = keys.map { $0.intValue.map { "[\($0)]" } ?? ".\($0.stringValue)" }.joined().trimmingCharacters(in: ["."])
            return .failure(Failure(message: "The report\(path.isEmpty ? "" : " field \(path)") \(why)"))
        }
    }

    static func decodeEnvelope(_ data: Data, command: String) -> Result<QuotaReport, Failure> {
        do { return .success(try QuotaReport.decodeEnvelope(data, command: command)) } catch {
            return .failure(Failure(message: "The quota command returned an unreadable response: \(error.localizedDescription)"))
        }
    }

}
