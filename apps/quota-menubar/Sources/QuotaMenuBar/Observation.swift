import Foundation

/// One scheduled callback per window, measured from its first event. Later events cannot postpone it.
@MainActor final class ObservationCoalescer {
    typealias Scheduler = @MainActor (TimeInterval, @escaping @MainActor () -> Void) -> () -> Void
    private let schedule: Scheduler, changed: () -> Void
    private var cancel: (() -> Void)?

    init(schedule: @escaping Scheduler = ObservationCoalescer.mainSchedule, changed: @escaping () -> Void) {
        self.schedule = schedule
        self.changed = changed
    }

    static func mainSchedule(_ delay: TimeInterval, _ action: @escaping @MainActor () -> Void) -> () -> Void {
        let work = DispatchWorkItem { MainActor.assumeIsolated { action() } }
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
        return { work.cancel() }
    }

    func invalidate() {
        guard cancel == nil else { return }
        cancel = schedule(0.25) { [weak self] in
            guard let self else { return }
            self.cancel = nil
            self.changed()
        }
    }

    func stop() { cancel?(); cancel = nil }
}

/// Filesystem metadata is an invalidation hint; the CLI remains the owner of the data.
@MainActor final class OverviewWatcher {
    private let root: URL
    private let coverageChanged: (String?) -> Void, opener: (String) -> Int32, projectWatchLimit: Int
    private var sources: [String: DispatchSourceFileSystemObject] = [:]
    private var stamps: [String: Stamp] = [:]
    private var coalescer: ObservationCoalescer?
    private var retry: Timer?
    private var stopped = false
    private var coverage: String?

    private struct Stamp: Equatable {
        let inode: UInt64, size: Int64, seconds: Int, nanos: Int
        init?(_ path: String) {
            var info = stat()
            guard stat(path, &info) == 0 else { return nil }
            inode = info.st_ino; size = info.st_size
            seconds = info.st_mtimespec.tv_sec; nanos = info.st_mtimespec.tv_nsec
        }
    }

    init(root: URL, coverageChanged: @escaping (String?) -> Void = { _ in },
         projectWatchLimit: Int = 64, opener: @escaping (String) -> Int32 = { open($0, O_EVTONLY) },
         schedule: @escaping ObservationCoalescer.Scheduler = ObservationCoalescer.mainSchedule,
         changed: @escaping () -> Void) {
        self.root = root.standardized
        self.coverageChanged = coverageChanged
        self.opener = opener
        self.projectWatchLimit = max(0, projectWatchLimit)
        coalescer = ObservationCoalescer(schedule: schedule, changed: changed)
        rebuild(notify: false)
        // One bounded recovery attempt per minute for missing directories or unavailable descriptors.
        let timer = Timer(timeInterval: 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.rebuild(notify: true) }
        }
        timer.tolerance = 5
        retry = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    deinit {
        retry?.invalidate()
        sources.values.forEach { $0.cancel() }
    }

    func stop() {
        guard !stopped else { return }
        stopped = true
        retry?.invalidate(); retry = nil
        coalescer?.stop(); coalescer = nil
        sources.values.forEach { $0.cancel() }
        sources.removeAll()
    }

    func rebuild(notify: Bool) {
        guard !stopped else { return }
        let databases = [root.appending(path: "run.sqlite3").path, root.appending(path: "fabric/host.sqlite3").path,
                         root.appending(path: "fabric/fabric.sqlite3").path].flatMap { [$0, $0 + "-wal"] }
        let projects = root.appending(path: "projects")
        let projectDirs = ((try? FileManager.default.contentsOfDirectory(at: projects, includingPropertiesForKeys: nil)) ?? [])
            .sorted { $0.path < $1.path }
        // Stat every project, even those beyond the watch cap, so the minute fallback can recover missed changes.
        let files = databases + projectDirs.map { $0.appending(path: "project.json").path }
        let now = Dictionary(uniqueKeysWithValues: files.compactMap { path in Stamp(path).map { (path, $0) } })
        let modified = now != stamps
        for path in Set(stamps.keys).union(now.keys) where stamps[path]?.inode != now[path]?.inode {
            sources.removeValue(forKey: path)?.cancel()
        }
        stamps = now
        // An existing ancestor observes creation of a missing root or fabric directory.
        var wanted = Set(databases.filter { now[$0] != nil })
        for directory in [root, root.appending(path: "fabric"), projects] + Array(projectDirs.prefix(projectWatchLimit)) {
            var ancestor = directory
            while !FileManager.default.fileExists(atPath: ancestor.path), ancestor.path != "/" {
                ancestor.deleteLastPathComponent()
            }
            wanted.insert(ancestor.path)
        }
        for path in Array(sources.keys) where !wanted.contains(path) {
            sources.removeValue(forKey: path)?.cancel()
        }
        var shortfall = projectDirs.count > projectWatchLimit
        for path in wanted.sorted() where sources[path] == nil {
            let fd = opener(path)
            guard fd >= 0 else { shortfall = true; continue }
            let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .delete, .rename, .extend], queue: .main)
            source.setEventHandler { [weak self, weak source] in
                MainActor.assumeIsolated {
                    guard let self, let source else { return }
                    if !source.data.isDisjoint(with: [.delete, .rename]) {
                        self.sources.removeValue(forKey: path)?.cancel()
                    }
                    self.rebuild(notify: true)
                }
            }
            source.setCancelHandler { close(fd) }
            sources[path] = source
            source.resume()
        }
        let newCoverage = shortfall ? "Some changes are checked only once a minute" : nil
        if newCoverage != coverage {
            coverage = newCoverage
            coverageChanged(newCoverage)
        }
        if notify && modified { coalescer?.invalidate() }
    }
}

/// Serialises overview reads and remembers an invalidation that arrived during a read.
struct OverviewReadGate {
    private(set) var reading = false
    private var pending = false

    mutating func begin(force: Bool) -> Bool {
        if reading { if force { pending = true }; return false }
        reading = true
        return true
    }

    mutating func finish() -> Bool {
        reading = false
        let rerun = pending
        pending = false
        return rerun
    }
}
