import XCTest
@testable import QuotaMenuBar

@MainActor
final class OverviewModelTests: XCTestCase {
    func testSingleFlightRerunAndRetainedError() {
        var reads: [(QuotaModel.OverviewResult) -> Void] = []
        var scheduled: [() -> Void] = []
        var now = Date(timeIntervalSince1970: 100)
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append($0) }, now: { now })
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
        scheduled.removeFirst()()
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
        var scheduled: [() -> Void] = []
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append($0) })
        model.refreshLanes(force: true)
        model.refreshLanes(force: true)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "failed"))))
        model.stop()
        scheduled.removeFirst()()
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
        var scheduled: [() -> Void] = []
        let model = QuotaModel(overviewReader: { _, completion in reads.append(completion) },
                               schedule: { scheduled.append($0) })
        model.observeOverviewRoot()
        XCTAssertNotEqual(model.observedRoot, root.standardized)
        model.refreshLanes(force: true)
        model.show(report)
        XCTAssertEqual(model.observedRoot, root.standardized)
        XCTAssertEqual(reads.count, 1)
        reads.removeFirst()((nil, .failure(CLI.Failure(message: "old root"))))
        XCTAssertEqual(scheduled.count, 1)
        scheduled.removeFirst()()
        XCTAssertEqual(reads.count, 1)
        model.stop()
    }
}
