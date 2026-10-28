import Foundation

/// Filesystem metadata is an invalidation hint; the CLI remains the owner of the data.
final class OverviewWatcher {
    private let root: URL, changed: () -> Void
    private var sources: [String: DispatchSourceFileSystemObject] = [:]
    private var stamps: [String: Stamp] = [:]
    private var debounce: DispatchWorkItem?
    private var retry: Timer?
    private var stopped = false

    private struct Stamp: Equatable {
        let inode: UInt64, size: Int64, seconds: Int, nanos: Int
        init?(_ path: String) {
            var info = stat()
            guard stat(path, &info) == 0 else { return nil }
            inode = info.st_ino; size = info.st_size
            seconds = info.st_mtimespec.tv_sec; nanos = info.st_mtimespec.tv_nsec
        }
    }

    init(root: URL, changed: @escaping () -> Void) {
        self.root = root.standardizedFileURL
        self.changed = changed
        rebuild(notify: false)
        // One bounded recovery attempt per minute for missing directories or unavailable descriptors.
        let timer = Timer(timeInterval: 60, repeats: true) { [weak self] _ in self?.rebuild(notify: true) }
        timer.tolerance = 5
        retry = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    deinit { stop() }

    func stop() {
        guard !stopped else { return }
        stopped = true
        retry?.invalidate(); retry = nil
        debounce?.cancel(); debounce = nil
        sources.values.forEach { $0.cancel() }
        sources.removeAll()
    }

    private var files: [String] {
        let databases = [root.appending(path: "run.sqlite3").path, root.appending(path: "fabric/host.sqlite3").path,
                         root.appending(path: "fabric/fabric.sqlite3").path].flatMap { [$0, $0 + "-wal"] }
        let projects = root.appending(path: "projects")
        let entries = (try? FileManager.default.contentsOfDirectory(at: projects, includingPropertiesForKeys: nil)) ?? []
        return databases + entries.map { $0.appending(path: "project.json").path }
    }

    private func rebuild(notify: Bool) {
        guard !stopped else { return }
        let now = Dictionary(uniqueKeysWithValues: files.compactMap { path in Stamp(path).map { (path, $0) } })
        let modified = now != stamps
        for path in Set(stamps.keys).union(now.keys) where stamps[path]?.inode != now[path]?.inode {
            sources.removeValue(forKey: path)?.cancel()
        }
        stamps = now
        // An existing ancestor observes creation of a missing root or fabric directory.
        var wanted = Set(files.filter { now[$0] != nil })
        let projects = root.appending(path: "projects")
        let projectDirs = (try? FileManager.default.contentsOfDirectory(at: projects, includingPropertiesForKeys: nil)) ?? []
        for directory in [root, root.appending(path: "fabric"), projects] + projectDirs {
            var ancestor = directory
            while !FileManager.default.fileExists(atPath: ancestor.path), ancestor.path != "/" {
                ancestor.deleteLastPathComponent()
            }
            wanted.insert(ancestor.path)
        }
        for path in Array(sources.keys) where !wanted.contains(path) {
            sources.removeValue(forKey: path)?.cancel()
        }
        for path in wanted where sources[path] == nil {
            let fd = open(path, O_EVTONLY)
            guard fd >= 0 else { continue }
            let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .delete, .rename, .extend], queue: .main)
            source.setEventHandler { [weak self, weak source] in
                guard let self, let source else { return }
                if !source.data.isDisjoint(with: [.delete, .rename]) {
                    self.sources.removeValue(forKey: path)?.cancel()
                }
                self.rebuild(notify: true)
            }
            source.setCancelHandler { close(fd) }
            sources[path] = source
            source.resume()
        }
        if notify && modified { invalidate() }
    }

    private func invalidate() {
        debounce?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self, !self.stopped else { return }
            self.debounce = nil
            self.changed()
        }
        debounce = work
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(250), execute: work)
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
