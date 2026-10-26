import XCTest
@testable import QuotaMenuBar

final class LaneTests: XCTestCase {
    private let now = ISO8601DateFormatter().date(from: "2026-10-25T10:00:00Z")!

    private func row(_ id: String, _ state: String, queued: String, started: String? = nil, finished: String? = nil, output: String? = nil) -> String {
        """
        {"run_id": "run\(id)", "task_id": "t", "attempt_id": "att\(id)", "state": "\(state)", "applied_route": {"model": "model-\(id)"},
         "queued_at": "\(queued)", "started_at": \(started.map { "\"\($0)\"" } ?? "null"),
         "finished_at": \(finished.map { "\"\($0)\"" } ?? "null"), "last_output_at": \(output.map { "\"\($0)\"" } ?? "null")}
        """
    }

    private func page(_ rows: [String], next: String? = nil) -> Data {
        Data("""
        {"schema_version": 1, "command": "lanes", "status": "ok", "error": null,
         "data": {"lanes": [\(rows.joined(separator: ","))], "next_cursor": \(next.map { "\"\($0)\"" } ?? "null")}}
        """.utf8)
    }

    func testOrdersInputThenFailedThenStalledAndIgnoresTheRest() throws {
        let decoded = try Lane.decodePage(page([
            row("1", "running", queued: "2026-10-25T08:00:00.000000Z", started: "2026-10-25T08:01:00.000000Z", output: "2026-10-25T08:02:00.000000Z"),
            row("2", "failed", queued: "2026-10-25T09:00:00.000000Z", finished: "2026-10-25T09:40:00.000000Z"),
            row("3", "input_required", queued: "2026-10-25T07:00:00.000000Z", finished: "2026-10-25T09:00:00.000000Z"),
            row("4", "starting", queued: "2026-10-25T09:30:00.000000Z"),
            row("5", "succeeded", queued: "2026-10-25T09:00:00.000000Z", finished: "2026-10-25T09:50:00.000000Z"),
            row("6", "failed", queued: "2026-10-25T05:00:00.000000Z", finished: "2026-10-25T06:00:00.000000Z"),
            row("7", "queued", queued: "2026-10-25T09:59:00.000000Z"),
            row("8", "running", queued: "2026-10-25T09:00:00.000000Z", output: "2026-10-25T09:59:30.000000Z"),
        ])).lanes
        let shown = LaneBoard.attention(decoded, now: now)
        XCTAssertEqual(shown.map(\.lane.attemptID), ["att3", "att2", "att4", "att1"])
        XCTAssertEqual(shown.map(\.kind), [.inputRequired, .failed, .stalled, .stalled])
        XCTAssertEqual(shown[0].lane.route, "model-3")
    }

    func testNoLanesShowsNothing() throws {
        XCTAssertTrue(LaneBoard.attention(try Lane.decodePage(page([])).lanes, now: now).isEmpty)
    }

    func testProtectedReviewNeedsAttention() throws {
        let decoded = try Lane.decodePage(page([
            row("1", "review_required", queued: "2026-10-25T09:00:00Z", finished: "2026-10-25T09:40:00Z"),
        ])).lanes
        let shown = LaneBoard.attention(decoded, now: now)
        XCTAssertEqual(shown.map(\.lane.attemptID), ["att1"])
        XCTAssertEqual(shown.map(\.kind), [.failed])
    }

    func testPageCursorAndFailureEnvelope() throws {
        XCTAssertEqual(try Lane.decodePage(page([], next: "100")).next, "100")
        let failure = Data(#"{"schema_version":1,"command":"lanes","status":"error","data":null,"error":{"code":"X","message":"boom"}}"#.utf8)
        XCTAssertThrowsError(try Lane.decodePage(failure))
    }

    func testRunDirectoryFollowsTheStateRoot() {
        let lane = Lane(runID: "abc", attemptID: "d", state: "failed", route: "m", queuedAt: nil, startedAt: nil, finishedAt: nil, lastOutputAt: nil)
        let root = LaneBoard.stateRoot(report: nil, environment: ["PROVENANT_STATE_DIR": "/tmp/state"])
        XCTAssertEqual(LaneBoard.runDirectory(lane, stateRoot: root).path, "/tmp/state/runs/abc")
    }
}
