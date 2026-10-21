import AppKit
import Observation

/// Owns the one refresh timer and at most one in-flight CLI run.
@MainActor @Observable
final class QuotaModel {
    private(set) var report: QuotaReport?
    private(set) var error: String?
    private(set) var updatedAt: Date?
    private(set) var isRefreshing = false
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var launcher: CLI.Launcher?   // resolved once, forgotten if it stops working
    @ObservationIgnored private let interval = max(
        ProcessInfo.processInfo.environment["QUOTA_INTERVAL_SECONDS"].flatMap(Double.init) ?? 120, 5)

    init() {
        // No timer while asleep. On wake, give the network a few seconds before fetching.
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.timer?.invalidate() }
        }
        center.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            DispatchQueue.main.asyncAfter(deadline: .now() + 5) { MainActor.assumeIsolated { self?.start() } }
        }
        start()
    }

    private func start() {
        timer?.invalidate()
        timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.refresh() } }
        timer?.tolerance = interval * 0.2   // lets the system coalesce wakeups
        RunLoop.main.add(timer!, forMode: .common)
        refresh()
    }

    func refresh() {
        guard !isRefreshing else { return }
        isRefreshing = true
        let cached = launcher
        Task {
            let (resolved, result) = await withCheckedContinuation { done in
                DispatchQueue.global(qos: .utility).async { done.resume(returning: CLI.quota(cached)) }
            }
            launcher = resolved
            isRefreshing = false
            switch result {
            case .success(let report): (self.report, error, updatedAt) = (report, nil, .now)
            case .failure(let failure): error = failure.message   // the last good report stays, flagged
            }
        }
    }

    /// Opening the panel refreshes data older than half an interval.
    func refreshIfStale() {
        if let updatedAt, Date.now.timeIntervalSince(updatedAt) < interval / 2 { return }
        refresh()
    }
}

/// Runs `provenant quota --json` off the main thread. The app never reads credentials itself.
enum CLI {
    struct Failure: Error { let message: String }
    struct Launcher: Sendable { let url: URL, path: String?, custom: Bool }
    struct Output { let status: Int32, signalled: Bool, timedOut: Bool, out: Data, err: String }

    /// State shared by the reader, watchdog and calling threads.
    final class Shared: @unchecked Sendable {
        private let lock = NSLock()
        private var err = Data(), timedOut = false
        func set(err data: Data) { lock.withLock { err = data } }
        func markTimedOut() { lock.withLock { timedOut = true } }
        var snapshot: (Data, Bool) { lock.withLock { (err, timedOut) } }
    }

    static func quota(_ cached: Launcher?) -> (Launcher?, Result<QuotaReport, Failure>) {
        guard let launcher = cached ?? resolve() else {
            return (nil, .failure(Failure(message: "provenant is not on the login shell's PATH. Set PROVENANT_BIN.")))
        }
        let name = launcher.custom ? "PROVENANT_BIN (\(launcher.url.path))" : "provenant"
        let run: Output
        do { run = try exec(launcher.url, ["quota", "--json"], path: launcher.path, limit: 60) } catch {
            return (nil, .failure(Failure(message: "Could not start \(name): \(error.localizedDescription)")))
        }
        let detail = run.err.split(separator: "\n").last.map(String.init) ?? "no error output"
        if run.timedOut { return (launcher, .failure(Failure(message: "provenant quota timed out after 60 seconds."))) }
        if run.signalled { return (launcher, .failure(Failure(message: "provenant quota was killed by signal \(run.status)."))) }
        if run.status == 127 {   // the launcher or its interpreter is gone: re-resolve next time
            let hint = launcher.custom ? "" : " Check that the login shell's PATH reaches it, or set PROVENANT_BIN."
            return (nil, .failure(Failure(message: "\(name) exited with status 127: \(detail).\(hint)")))
        }
        if run.status != 0 { return (launcher, .failure(Failure(message: "\(name) exited with status \(run.status): \(detail)"))) }
        return (launcher, decode(run.out))
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

    /// Drains both pipes before waiting, so neither can fill and block the child; a watchdog ends overruns.
    static func exec(_ url: URL, _ arguments: [String], path: String?, limit: Double) throws -> Output {
        let process = Process(), out = Pipe(), err = Pipe(), shared = Shared()
        process.executableURL = url
        process.arguments = arguments
        if let path { process.environment = ProcessInfo.processInfo.environment.merging(["PATH": path]) { $1 } }
        (process.standardOutput, process.standardError, process.standardInput) = (out, err, FileHandle.nullDevice)
        try process.run()
        let drained = DispatchGroup()
        DispatchQueue.global(qos: .utility).async(group: drained) { shared.set(err: err.fileHandleForReading.readDataToEndOfFile()) }
        let watchdog = DispatchWorkItem { if process.isRunning { shared.markTimedOut(); process.terminate() } }
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + limit, execute: watchdog)
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        watchdog.cancel()
        drained.wait()
        let (errData, timedOut) = shared.snapshot
        return Output(status: process.terminationStatus, signalled: process.terminationReason == .uncaughtSignal,
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
