import AppKit
import IOKit.ps
import Network
import notify
import os
import ProvenantKit

let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "dev.provenant.quota-menubar", category: "refresh")

/// What asked for a refresh, as `provenant quota tick --trigger` names it.
enum Trigger: String {
    case timer, panel, manual, wake, power

    /// Coalesce events arriving during a refresh; due timers need no replay.
    func queued(after pending: Trigger?) -> Trigger? {
        let strength: [Trigger] = [.power, .panel, .wake, .manual]
        guard let rank = strength.firstIndex(of: self) else { return pending }
        return rank > pending.flatMap(strength.firstIndex) ?? -1 ? self : pending
    }
}

/// Watches the current report and runs `provenant quota tick` when an account is due or something happens.
@MainActor @Observable
final class QuotaModel {
    private(set) var report: QuotaReport?
    private(set) var error: String?
    private(set) var updatedAt: Date?
    private(set) var isRefreshing = false
    private(set) var overview: ConsoleOverview?
    private(set) var overviewError: String?
    private(set) var overviewObservationError: String?
    private(set) var overviewUpdatedAt: Date?
    @ObservationIgnored private var overviewGate = OverviewReadGate()
    @ObservationIgnored private var lanesReadAt: TimeInterval?
    @ObservationIgnored private var overviewWatcher: OverviewWatcher?
    private(set) var observedRoot: URL?
    @ObservationIgnored private var overviewRerunScheduled = false
    @ObservationIgnored private var stopped = false
    typealias OverviewResult = (CLI.Launcher?, Result<ConsoleOverview, CLI.Failure>)
    typealias OverviewReader = (CLI.Launcher?, @escaping (OverviewResult) -> Void) -> Void
    @ObservationIgnored private var overviewReader: OverviewReader = { cached, completion in
        Task { @MainActor in
            let result = await Task.detached(priority: .background) { CLI.overview(cached) }.value
            completion(result)
        }
    }
    @ObservationIgnored private var scheduleOverview: (TimeInterval, @escaping () -> Void) -> Void = { delay, work in
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: DispatchWorkItem(block: work))
    }
    @ObservationIgnored private var overviewNow: () -> Date = { .now }
    @ObservationIgnored private var overviewUptime: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    private let overviewReadInterval: TimeInterval = 2
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

    /// A controlled reader and scheduler exercise the same refresh path without a CLI or timers.
    init(overviewReader: @escaping OverviewReader, schedule: @escaping (TimeInterval, @escaping () -> Void) -> Void,
         now: @escaping () -> Date = { .now }, uptime: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.overviewReader = overviewReader
        self.scheduleOverview = schedule
        self.overviewNow = now
        self.overviewUptime = uptime
    }

    init() {
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.timer?.invalidate(); self?.clock?.invalidate() }
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
        observeOverviewRoot()
        refreshLanes(force: true)
        loadCurrent()
    }

    /// One read at a time; an invalidation during the read always gets a follow-up.
    func refreshLanes(force: Bool = false) {
        guard !fixture, !stopped, !overviewRerunScheduled else { return }
        if !force, !overviewGate.reading, let at = lanesReadAt, overviewUptime() - at < 10 { return }
        // An in-flight read owns the scope. Remember forced changes and service them once it completes.
        if overviewGate.reading { _ = overviewGate.begin(force: force); return }
        if force, let at = lanesReadAt {
            let remaining = overviewReadInterval - (overviewUptime() - at)
            if remaining > 0 {
                overviewRerunScheduled = true
                scheduleOverview(remaining) { [weak self] in
                    guard let self, !self.stopped else { return }
                    self.overviewRerunScheduled = false
                    self.refreshLanes(force: true)
                }
                return
            }
        }
        guard overviewGate.begin(force: force) else { return }
        lanesReadAt = overviewUptime()
        let cached = launcher
        overviewReader(cached) { [weak self] result in
            guard let self, !self.stopped else { return }
            let (resolved, outcome) = result
            let rerun = self.overviewGate.finish()
            self.launcher = self.launcher ?? resolved
            switch outcome {
            case .success(let found): self.overview = found; self.overviewError = nil; self.overviewUpdatedAt = self.overviewNow()
            case .failure(let failure): self.overviewError = failure.message; log.error("\(failure.message, privacy: .public)")
            }
            if rerun { self.refreshLanes(force: true) }
        }
    }

    /// Retry a failed read on the watcher's minute recovery tick, even if no files changed.
    func retryFailedOverview() {
        if overviewError != nil { refreshLanes(force: true) }
    }

    func observeOverviewRoot() {
        guard !fixture, !stopped else { return }
        let root = stateRoot.standardized
        guard root != observedRoot else { return }
        overviewWatcher?.stop()
        overviewObservationError = nil
        observedRoot = root
        overviewWatcher = OverviewWatcher(root: root, coverageChanged: { [weak self] error in
            self?.overviewObservationError = error
        }, onMinute: { [weak self] in self?.retryFailedOverview() }) { [weak self] in self?.refreshLanes(force: true) }
    }

    func stop() {
        guard !stopped else { return }
        stopped = true
        overviewWatcher?.stop(); overviewWatcher = nil
        watcher?.stop(); watcher = nil
        timer?.invalidate(); clock?.invalidate()
        network.cancel()
        if powerToken != 0 { notify_cancel(powerToken); powerToken = 0 }
    }

    var stateRoot: URL { LaneBoard.stateRoot(report: report) }

    /// ⌘R.
    func refresh() { refreshLanes(force: true); run(.manual) }

    /// Each menu open; avoid closely spaced ticks.
    func panelOpened() {
        refreshLanes()
        if !isRefreshing, let updatedAt, Date.now.timeIntervalSince(updatedAt) < 5 { return }
        run(.panel)
    }

    /// A wake always ticks so the CLI can refresh due accounts.
    private func woke() {
        relabel()
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
            let (resolved, outcome) = await Task.detached(priority: .utility) {
                CLI.quota(cached, ["quota", "show", "--json"])
            }.value
            guard !stopped else { return }
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
            if let next = pending { pending = nil; run(next) }
        }
    }

    /// Runs a `provenant` settings command (a display choice or `accounts set`), then reloads `quota show` so the
    /// menu and the status item follow at once. A failure shows as the menu's refresh error.
    func apply(_ arguments: [String]) {
        guard !fixture else { return }
        let cached = launcher
        Task {
            let (resolved, failure) = await Task.detached(priority: .userInitiated) { CLI.command(cached, arguments) }.value
            guard !stopped else { return }
            launcher = resolved ?? launcher
            if let failure {
                error = failure.message
                relabel()
                return
            }
            let (found, outcome) = await Task.detached(priority: .userInitiated) {
                CLI.quota(resolved ?? cached, ["quota", "show", "--json"])
            }.value
            guard !stopped else { return }
            launcher = found ?? launcher
            switch outcome {
            case .report(let report): show(report)
            case .failure(let failure): error = failure.message; relabel()
            }
        }
    }

    private func run(_ trigger: Trigger) {
        guard !fixture, !stopped else { return }
        guard !isRefreshing else {
            pending = trigger.queued(after: pending)
            return
        }
        isRefreshing = true
        let arguments = ["quota", "tick", "--trigger", trigger.rawValue, "--power", power.rawValue, "--json"]
        log.info("tick: trigger \(trigger.rawValue, privacy: .public), power \(self.power.rawValue, privacy: .public)")
        let cached = launcher
        Task {
            let (resolved, outcome) = await Task.detached(priority: .utility) {
                CLI.quota(cached, arguments)
            }.value
            guard !stopped else { return }
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
    func show(_ incoming: QuotaReport) {
        let oldRoot = observedRoot
        let report = self.report.flatMap { shown in incoming.generatedAt < shown.generatedAt ? shown : nil } ?? incoming
        (self.report, error, updatedAt) = (report, nil, .now)
        observeOverviewRoot()
        if oldRoot != nil, oldRoot != observedRoot { refreshLanes(force: true) }
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
        let timer = Timer(timeInterval: due.timeIntervalSince(now) + 1, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.relabel() }
        }
        timer.tolerance = 5
        clock = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    /// One timer at the earliest due time, but no sooner than 30 s, so a report that is always due cannot spin.
    private func schedule(_ due: Date?) {
        timer?.invalidate()
        let delay = max(due?.timeIntervalSinceNow ?? retryInterval, 30)
        let timer = Timer(timeInterval: delay, repeats: false) { [weak self] _ in MainActor.assumeIsolated { self?.run(.timer) } }
        timer.tolerance = min(delay * 0.1, 60)   // lets the system coalesce wakeups
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
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
@MainActor final class ReportWatcher {
    private let file: URL, changed: (Data) -> Void
    private var source: DispatchSourceFileSystemObject?
    private var parent: DispatchSourceFileSystemObject?
    private var parentPath: String?
    private var seen: [Int]?
    private var stopped = false

    init(file: URL, changed: @escaping (Data) -> Void) {
        (self.file, self.changed) = (file, changed)
        watchParent()
        arm()
    }

    deinit { source?.cancel(); parent?.cancel() }

    func stop() { stopped = true; source?.cancel(); source = nil; parent?.cancel(); parent = nil }

    private func watchParent() {
        guard !stopped else { return }
        var ancestor = file.deletingLastPathComponent().deletingLastPathComponent()
        while !FileManager.default.fileExists(atPath: ancestor.path), ancestor.path != "/" {
            ancestor.deleteLastPathComponent()
        }
        if parent != nil, parentPath == ancestor.path { return }
        parent?.cancel(); parent = nil; parentPath = nil
        let fd = open(ancestor.path, O_EVTONLY)
        guard fd >= 0 else { return }
        let parent = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .delete, .rename], queue: .main)
        parent.setEventHandler { [weak self] in
            guard let self else { return }
            if let events = self.parent?.data, !events.isDisjoint(with: [.delete, .rename]) {
                self.parent?.cancel(); self.parent = nil; self.parentPath = nil
                self.source?.cancel(); self.source = nil
            }
            self.watchParent()
            self.arm()
        }
        parent.setCancelHandler { close(fd) }
        self.parent = parent
        parentPath = ancestor.path
        parent.resume()
    }

    /// Opens the report directory; it may not exist until the first tick creates it.
    func arm() {
        guard !stopped else { return }
        if parent == nil { watchParent() }
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
enum Power: String {
    case ac
    case battery
    case low

    static var current: Power {
        if ProcessInfo.processInfo.isLowPowerModeEnabled { return .low }
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let source = IOPSGetProvidingPowerSourceType(info)?.takeUnretainedValue() else { return .ac }
        return source as String == kIOPSBatteryPowerValue ? .battery : .ac
    }
}
