import XCTest
@testable import QuotaMenuBar

@MainActor
final class OverviewModelTests: XCTestCase {
    func testSingleFlightRerunAndRetainedError() {
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        var scheduled: [(TimeInterval, () -> Void)] = []
        var now = Date(timeIntervalSince1970: 100)
        var uptime: TimeInterval = 0
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append(($0, $1)) }, now: { now }, uptime: { uptime })
        let good = ConsoleOverview(needsYou: [], lanes: [], counts: .init(needsYou: 0, running: 0, attention: 0),
                                   coverage: .init(scope: "local", inbox: "complete", runs: "complete", nestedRuns: "unavailable"))

        model.refreshLanes(force: true)
        model.refreshLanes(force: true)
        model.refreshLanes(force: true)
        XCTAssertEqual(reads.count, 1)
        reads.removeFirst()((nil, .success(good)))
        XCTAssertEqual(scheduled.count, 1)
        XCTAssertEqual(model.overview, good)
        XCTAssertEqual(model.overviewUpdatedAt, now)
        // Further events before the scheduled read do not spawn a second read or another timer.
        model.refreshLanes(force: true)
        XCTAssertEqual(reads.count, 0)
        XCTAssertEqual(scheduled.count, 1)
        XCTAssertEqual(scheduled[0].0, 2)
        uptime = 2
        scheduled.removeFirst().1()
        XCTAssertEqual(reads.count, 1)
        now.addTimeInterval(20)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "failed"))))
        XCTAssertEqual(model.overview, good)
        XCTAssertEqual(model.overviewError, "failed")
        XCTAssertEqual(model.overviewUpdatedAt, Date(timeIntervalSince1970: 100))
        XCTAssertTrue(scheduled.isEmpty)
        model.stop()
    }

    func testShutdownDiscardsScheduledRerun() {
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        var scheduled: [(TimeInterval, () -> Void)] = []
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append(($0, $1)) })
        model.refreshLanes(force: true)
        model.refreshLanes(force: true)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "failed"))))
        model.stop()
        scheduled.removeFirst().1()
        XCTAssertTrue(reads.isEmpty)
    }

    func testReportRerootsOverviewWatcher() throws {
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().appending(path: "Fixtures/current.json")
        var json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: directory)) as? [String: Any])
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        json["report_path"] = root.appending(path: "quota/report.json").path
        let report = try QuotaReport.decode(JSONSerialization.data(withJSONObject: json))
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        var scheduled: [(TimeInterval, () -> Void)] = []
        var uptime: TimeInterval = 0
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append(($0, $1)) }, uptime: { uptime })
        model.observeOverviewRoot()
        XCTAssertNotEqual(model.observedRoot, root.standardized)
        model.refreshLanes(force: true)
        model.show(report)
        XCTAssertEqual(model.observedRoot, root.standardized)
        XCTAssertEqual(reads.count, 1)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "old root"))))
        XCTAssertEqual(scheduled.count, 1)
        uptime = 2
        scheduled.removeFirst().1()
        XCTAssertEqual(reads.count, 1)
        model.stop()
    }

    func testSustainedEventsRespectReadFloorAndLeaveOneTrailingRead() {
        var uptime: TimeInterval = 0
        var jobs: [(TimeInterval, () -> Void)] = []
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        var starts: [TimeInterval] = []
        let model = QuotaModel(overviewReader: { _, completion in starts.append(uptime); reads.append(completion) },
            schedule: { delay, work in jobs.append((uptime + delay, work)) }, uptime: { uptime })
        let overview = ConsoleOverview(needsYou: [], lanes: [], counts: .init(needsYou: 0, running: 0, attention: 0),
            coverage: .init(scope: "local", inbox: "complete", runs: "complete", nestedRuns: "unavailable"))
        model.refreshLanes(force: true)
        // Activity every 100 ms repeatedly invalidates both idle and in-flight reads.
        for step in 1...51 {
            uptime = Double(step) / 10
            if !reads.isEmpty { reads.removeFirst()((nil, .success(overview))) }
            model.refreshLanes(force: true)
            if let index = jobs.firstIndex(where: { $0.0 <= uptime }) {
                jobs.remove(at: index).1()
            }
        }
        XCTAssertEqual(starts, [0, 2, 4])
        XCTAssertEqual(jobs.count, 1)
        uptime = 6
        jobs.removeFirst().1()
        XCTAssertEqual(starts, [0, 2, 4, 6])
        reads.removeFirst()((nil, .success(overview)))
        XCTAssertTrue(jobs.isEmpty)
        model.stop()
    }

    func testMinuteRetriesFailedReadWithoutAChange() {
        var uptime: TimeInterval = 0
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { _, _ in XCTFail("No timer expected") }, uptime: { uptime })
        model.refreshLanes(force: true)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "failed"))))
        uptime = 60
        model.retryFailedOverview()
        XCTAssertEqual(reads.count, 1)
        reads.removeFirst()((nil, .success(.init(needsYou: [], lanes: [],
            counts: .init(needsYou: 0, running: 0, attention: 0),
            coverage: .init(scope: "local", inbox: "complete", runs: "complete", nestedRuns: "unavailable")))))
        model.retryFailedOverview()
        XCTAssertTrue(reads.isEmpty)
        model.stop()
    }

    func testLongReadRerunsWithoutAnotherDelay() {
        var uptime: TimeInterval = 0
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { _, _ in XCTFail("The read floor has elapsed") }, uptime: { uptime })
        model.refreshLanes(force: true)
        model.refreshLanes(force: true)
        uptime = 3
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "failed"))))
        XCTAssertEqual(reads.count, 1)
        model.stop()
    }
}
