import XCTest
@testable import QuotaMenuBar

@MainActor final class ObservationTests: XCTestCase {
    @MainActor private final class Scheduler {
        final class Job {
            let at: TimeInterval, action: @MainActor () -> Void
            var cancelled = false
            init(at: TimeInterval, action: @escaping @MainActor () -> Void) { self.at = at; self.action = action }
        }
        var now: TimeInterval = 0
        var jobs: [Job] = []
        func schedule(_ delay: TimeInterval, _ action: @escaping @MainActor () -> Void) -> () -> Void {
            let job = Job(at: now + delay, action: action)
            jobs.append(job)
            return { job.cancelled = true }
        }
        func advance(to time: TimeInterval) {
            while let job = jobs.filter({ !$0.cancelled && $0.at <= time }).min(by: { $0.at < $1.at }) {
                job.cancelled = true
                now = job.at
                job.action()
            }
            now = time
        }
    }

    func testSingleFlightRerunsOnce() {
        var gate = OverviewReadGate()
        XCTAssertTrue(gate.begin(force: true))
        XCTAssertFalse(gate.begin(force: true))
        XCTAssertFalse(gate.begin(force: true))
        XCTAssertTrue(gate.finish())
        XCTAssertTrue(gate.begin(force: true))
        XCTAssertFalse(gate.finish())
    }

    func testWatcherRecreationAndWAL() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let fabric = root.appending(path: "fabric")
        try FileManager.default.createDirectory(at: fabric, withIntermediateDirectories: true)
        let wal = fabric.appending(path: "fabric.sqlite3-wal")
        var count = 0
        let watcher = OverviewWatcher(root: root) { count += 1 }
        try Data("first".utf8).write(to: wal)
        waitUntil { count == 1 }
        try Data("second".utf8).write(to: wal, options: .atomic)
        waitUntil { count == 2 }
        try FileManager.default.removeItem(at: fabric)
        waitUntil { count == 3 }
        try FileManager.default.createDirectory(at: fabric, withIntermediateDirectories: true)
        try Data("third".utf8).write(to: wal)
        waitUntil { count == 4 }
        try Data("fourth".utf8).write(to: wal)
        waitUntil { count == 5 }
        watcher.stop()
        try Data("fifth".utf8).write(to: wal)
        RunLoop.main.run(until: Date().addingTimeInterval(0.4))
        XCTAssertEqual(count, 5)
    }

    func testMissingRootAndReplacement() throws {
        let parent = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: parent) }
        let root = parent.appending(path: "state")
        var count = 0
        let watcher = OverviewWatcher(root: root) { count += 1 }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data("first".utf8).write(to: root.appending(path: "run.sqlite3"))
        waitUntil { count == 1 }
        try FileManager.default.removeItem(at: root)
        waitUntil { count == 2 }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data("second".utf8).write(to: root.appending(path: "run.sqlite3"))
        waitUntil { count == 3 }
        watcher.stop()
    }

    func testCoalescesSustainedChangesWithoutPostponingDelivery() {
        let scheduler = Scheduler()
        var count = 0
        let coalescer = ObservationCoalescer(schedule: scheduler.schedule) { count += 1 }
        coalescer.invalidate()
        scheduler.advance(to: 0.1)
        coalescer.invalidate()
        scheduler.advance(to: 0.2)
        coalescer.invalidate()
        XCTAssertEqual(count, 0)
        scheduler.advance(to: 0.25)
        XCTAssertEqual(count, 1)
        coalescer.invalidate()
        scheduler.advance(to: 0.5)
        XCTAssertEqual(count, 2)
        coalescer.invalidate()
        coalescer.stop()
        scheduler.advance(to: 1)
        XCTAssertEqual(count, 2)
    }

    func testWatchCapFallsBackToMetadataAndReportsShortfall() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let project = root.appending(path: "projects/a")
        try FileManager.default.createDirectory(at: project, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let scheduler = Scheduler()
        var coverage: String?, count = 0
        let watcher = OverviewWatcher(root: root, coverageChanged: { coverage = $0 }, projectWatchLimit: 0,
                                      schedule: scheduler.schedule) { count += 1 }
        XCTAssertNotNil(coverage)
        try Data("project".utf8).write(to: project.appending(path: "project.json"))
        watcher.minuteTick()
        XCTAssertEqual(count, 1)
        watcher.stop()
    }

    func testOpenFailureReportsShortfallAndRecovery() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        var fail = true, coverage: String?
        let watcher = OverviewWatcher(root: root, coverageChanged: { coverage = $0 }, opener: { path in
            fail ? -1 : open(path, O_EVTONLY)
        }) {}
        XCTAssertNotNil(coverage)
        fail = false
        watcher.minuteTick()
        XCTAssertNil(coverage)
        watcher.stop()
    }

    func testEventsCoalesceTheScanAndFailedOpensWaitForMinute() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let scheduler = Scheduler()
        var attempts: [String: Int] = [:], fail = true, minutes = 0, changes = 0
        let watcher = OverviewWatcher(root: root, opener: { path in
            attempts[path, default: 0] += 1
            return fail ? -1 : open(path, O_EVTONLY)
        }, schedule: scheduler.schedule, onMinute: { minutes += 1 }) { changes += 1 }
        let initial = attempts[root.path, default: 0]
        XCTAssertEqual(initial, 1)
        try Data("one".utf8).write(to: root.appending(path: "run.sqlite3"))
        for _ in 0..<50 { watcher.eventOccurred() }
        XCTAssertNil(attempts[root.appending(path: "run.sqlite3").path])
        scheduler.advance(to: 0.25)
        XCTAssertEqual(changes, 1)
        XCTAssertEqual(attempts[root.path], initial)
        XCTAssertEqual(attempts[root.appending(path: "run.sqlite3").path], 1)
        fail = false
        for _ in 0..<50 { watcher.eventOccurred() }
        scheduler.advance(to: 0.5)
        XCTAssertEqual(attempts[root.path], initial)
        XCTAssertEqual(attempts[root.appending(path: "run.sqlite3").path], 1)
        watcher.minuteTick()
        XCTAssertEqual(attempts[root.path], initial + 1)
        XCTAssertEqual(attempts[root.appending(path: "run.sqlite3").path], 2)
        XCTAssertEqual(minutes, 1)
        watcher.eventOccurred()
        watcher.stop()
        scheduler.advance(to: 1)
        XCTAssertEqual(changes, 1)
        watcher.minuteTick()
        XCTAssertEqual(minutes, 1)
    }

    func testReportWatcherMissingRoot() throws {
        let parent = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: parent) }
        let dir = parent.appending(path: "state/quota")
        let report = dir.appending(path: "report.json")
        var seen: [String] = []
        let watcher = ReportWatcher(file: report) { seen.append(String(decoding: $0, as: UTF8.self)) }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try Data("one".utf8).write(to: report)
        waitUntil { seen == ["one"] }
        withExtendedLifetime(watcher) {}
    }

    func testReportWatcherRecreation() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let dir = root.appending(path: "quota")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let report = dir.appending(path: "report.json")
        try Data("one".utf8).write(to: report)
        var seen: [String] = []
        let watcher = ReportWatcher(file: report) { seen.append(String(decoding: $0, as: UTF8.self)) }
        XCTAssertEqual(seen, ["one"])
        try FileManager.default.removeItem(at: dir)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try Data("two".utf8).write(to: report)
        waitUntil { seen.contains("two") }
        withExtendedLifetime(watcher) {}
    }

    private func waitUntil(_ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(3)
        while !condition() && Date() < deadline { RunLoop.main.run(until: Date().addingTimeInterval(0.02)) }
        XCTAssertTrue(condition())
    }
}
