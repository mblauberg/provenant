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
    @ObservationIgnored private let interval = max(
        ProcessInfo.processInfo.environment["QUOTA_INTERVAL_SECONDS"].flatMap(Double.init) ?? 120, 5)

    init() {
        // No timer while asleep; refresh once on wake.
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.timer?.invalidate() }
        }
        center.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.start() }
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
        Task {
            let result = await withCheckedContinuation { done in
                DispatchQueue.global(qos: .utility).async { done.resume(returning: Self.run()) }
            }
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

    struct Failure: Error { let message: String }

    /// Runs `provenant quota --json` off the main thread. The app never reads credentials itself.
    nonisolated private static func run() -> Result<QuotaReport, Failure> {
        let process = Process(), out = Pipe(), err = Pipe()
        if let bin = ProcessInfo.processInfo.environment["PROVENANT_BIN"], !bin.isEmpty {
            process.executableURL = URL(fileURLWithPath: bin)
            process.arguments = ["quota", "--json"]
        } else {   // a login shell supplies the user's PATH; exec so the watchdog kills provenant itself
            process.executableURL = URL(fileURLWithPath: "/bin/zsh")
            process.arguments = ["-lc", "exec provenant quota --json"]
        }
        (process.standardOutput, process.standardError, process.standardInput) = (out, err, FileHandle.nullDevice)
        do { try process.run() } catch { return .failure(Failure(message: "Could not start provenant: \(error.localizedDescription)")) }

        // Drain both pipes before waiting, so neither can fill and block the child.
        var errData = Data()
        let drained = DispatchGroup()
        DispatchQueue.global(qos: .utility).async(group: drained) { errData = err.fileHandleForReading.readDataToEndOfFile() }
        let watchdog = DispatchWorkItem { if process.isRunning { process.terminate() } }
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 60, execute: watchdog)
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        watchdog.cancel()
        drained.wait()

        let lastLine = String(decoding: errData, as: UTF8.self).split(separator: "\n").last.map(String.init)
        switch (process.terminationReason, process.terminationStatus) {
        case (.uncaughtSignal, _): return .failure(Failure(message: "provenant quota timed out after 60 seconds."))
        case (_, 0): break
        case (_, 127): return .failure(Failure(message: "provenant is not on the login shell's PATH. Set PROVENANT_BIN."))
        case (_, let code): return .failure(Failure(message: "provenant quota exited with status \(code): \(lastLine ?? "no error output")"))
        }
        do {
            let report = try QuotaReport.decode(data)
            guard report.schema == QuotaReport.schemaID else { return .failure(Failure(message: "Unsupported report schema \(report.schema).")) }
            return .success(report)
        } catch let DecodingError.keyNotFound(key, context) {
            return .failure(Failure(message: "The report is missing \(path(context.codingPath + [key]))."))
        } catch let DecodingError.typeMismatch(_, context), let DecodingError.valueNotFound(_, context),
                let DecodingError.dataCorrupted(context) {
            return .failure(Failure(message: "The report has an unreadable \(path(context.codingPath)). \(context.debugDescription)"))
        } catch {
            return .failure(Failure(message: "Could not read the report: \(error.localizedDescription)"))
        }
    }

    nonisolated private static func path(_ keys: [CodingKey]) -> String {
        keys.isEmpty ? "document" : keys.map { $0.intValue.map { "[\($0)]" } ?? ".\($0.stringValue)" }.joined()
            .trimmingCharacters(in: ["."])
    }
}
