import XCTest
@testable import QuotaMenuBar

final class ObservationTests: XCTestCase {
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

    func testCoalescesBurst() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        var count = 0
        let watcher = OverviewWatcher(root: root) { count += 1 }
        let db = root.appending(path: "run.sqlite3")
        try Data("one".utf8).write(to: db)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        try Data("two".utf8).write(to: db)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        XCTAssertEqual(count, 0)
        waitUntil { count == 1 }
        watcher.stop()
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
