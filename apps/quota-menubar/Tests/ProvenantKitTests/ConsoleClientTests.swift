import Foundation
import XCTest
import ProvenantTestSupport
@testable import ProvenantKit

final class ConsoleClientTests: XCTestCase {
    private func client(_ replies: [Replay.Reply]) -> (ConsoleClient, Replay) {
        let replay = Replay(replies)
        return (ConsoleClient(cli: replay.client), replay)
    }

    func testOverviewKeepsNullCountsUnknownAndNamesCoverage() async throws {
        let (active, _) = client([(["console", "overview"], "console-overview-active", 0)])
        let lanes = try await active.overview().lanes
        XCTAssertEqual(lanes.map(\.title), ["implement-console-overview", "review-the-change-set", "run-the-whole-test-suite"])
        XCTAssertEqual(lanes.last?.state, "failed")
        XCTAssertEqual(lanes.last?.attention, true)
        XCTAssertNotNil(lanes.last?.key)
        XCTAssertEqual(lanes.first?.projectLabel, "provenant")

        let (console, _) = client([(["console", "overview"], "console-overview", 0)])
        let overview = try await console.overview()
        XCTAssertEqual(overview.counts.running, 0)
        XCTAssertEqual(overview.counts.attention, 0)
        XCTAssertEqual(overview.counts.needsYou, 1)
        XCTAssertEqual(overview.needsYou.map(\.title), ["Review console"])
        XCTAssertEqual(overview.needsYou.first?.projectLabel, "project")
        // Nested runs are out of scope by design: a quiet footnote, never a coverage warning.
        XCTAssertEqual(overview.coverage.notices, [])
        XCTAssertEqual(overview.coverage.nestedRunsFootnote, "Nested runs are not included here.")
    }

    func testPartialCoverageIsInformationAndUnavailableCoverageIsAWarning() throws {
        func coverage(_ json: String) throws -> OverviewCoverage {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(OverviewCoverage.self, from: Data(json.utf8))
        }
        let partial = try coverage(#"{"inbox":"complete","runs":"partial","nested_runs":"unavailable"}"#)
        XCTAssertEqual(partial.notices, [.info("Some runs may be missing from these counts.")])
        XCTAssertEqual(partial.nestedRunsFootnote, "Nested runs are not included here.")
        let unavailable = try coverage(#"{"inbox":"complete","runs":"unavailable","nested_runs":"unavailable"}"#)
        XCTAssertEqual(unavailable.notices, [.warning("Run state is unavailable here.")])
        let complete = try coverage(#"{"inbox":"complete","runs":"complete","nested_runs":"unavailable"}"#)
        XCTAssertTrue(complete.notices.isEmpty)
    }

    func testLaneTitleComesFromTheKeyNotTheTaskLabel() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = #"""
        {"coverage":{"inbox":"complete","runs":"complete","nested_runs":"unavailable","scope":"local"},
         "counts":{},"needs_you":[],
         "lanes":[{"project_id":"p","project_label":"P","attempt_id":"a1","task_label":"task",
                   "key":{"attempt":1,"graph":"lane-default","node":"task","project":"p"},
                   "state":"running","attention":false},
                  {"project_id":"p","project_label":"P","attempt_id":"a2","task_label":"task",
                   "key":{"attempt":2,"graph":"lane-retry","node":"task--writer","project":"p"},
                   "state":"running","attention":false}],
         "schema_version":2}
        """#
        let overview = try decoder.decode(Overview.self, from: Data(json.utf8))
        XCTAssertEqual(overview.lanes.map(\.title), ["lane-default", "lane-retry · task--writer · attempt 2"])
    }

    func testBoardDecodesLanesAndPassesPagingArguments() async throws {
        let (console, replay) = client([(["console", "board"], "console-board", 0)])
        let board = try await console.board(all: true, limit: 25, cursor: "C1")
        XCTAssertEqual(board.lanes.count, 1)
        let lane = try XCTUnwrap(board.lanes.first)
        XCTAssertEqual(lane.title, "board-fixture")
        XCTAssertEqual(lane.metadata.attempt.state, "queued")
        XCTAssertEqual(lane.laneId.key?.graph, "board-fixture")
        XCTAssertEqual(lane.metadata.attempt.requestedRoute?.summary, "fixture/model · high")
        XCTAssertEqual(lane.wait?.waitingFor, "launch: not yet handed to a guardian")
        XCTAssertEqual(board.highWater, 101)
        XCTAssertEqual(replay.recorded.first, ["console", "board", "--limit", "25", "--all", "--cursor", "C1", "--json"])
    }

    func testEmptyBoardDecodes() async throws {
        let (console, _) = client([(["console", "board"], "console-board-empty", 0)])
        let board = try await console.board()
        XCTAssertTrue(board.lanes.isEmpty)
        XCTAssertEqual(board.coverage.runs, "empty")
    }

    func testDetailDecodesUsageGapsAsMissingNotZero() async throws {
        let (console, replay) = client([(["console", "detail"], "console-detail", 0)])
        let detail = try await console.detail("6565")
        XCTAssertTrue(detail.isFound)
        XCTAssertEqual(detail.artifacts?.map(\.name), ["prompt"])
        XCTAssertEqual(detail.usage?.counts.count, 0)
        XCTAssertEqual(detail.usageCoverage?.missing.count, 5)
        XCTAssertEqual(detail.timings?.startedAt, nil)
        XCTAssertEqual(replay.recorded.first, ["console", "detail", "6565", "--json"])
    }

    func testProjectsAndSessionsDecodeWithAndWithoutSummaries() async throws {
        let (console, _) = client([(["console", "projects"], "console-projects", 0), (["console", "sessions"], "console-sessions", 0)])
        let projects = try await console.projects().projects
        XCTAssertEqual(projects.map(\.label), ["project"])
        XCTAssertEqual(projects.first?.commonDir?.path, "/fixture/project/.git")
        let sessions = try await console.sessions().sessions
        XCTAssertEqual(sessions.first?.summaryLine, "Implementing console")
        XCTAssertEqual(sessions.first?.record.summary?.now, "Implementing console")
        XCTAssertEqual(sessions.first?.summarySource, "structured")
        XCTAssertEqual(sessions.first?.id, "console-chair")

        let (bare, _) = client([(["console", "sessions"], "console-sessions-without-summary", 0)])
        let derived = try await bare.sessions().sessions
        XCTAssertFalse(derived.isEmpty)
        XCTAssertTrue(derived.allSatisfy { $0.summaryLine == "Console checkpoint" })
        XCTAssertNil(derived.first?.record.summary)
        XCTAssertTrue(derived.allSatisfy { $0.summarySource == "title" })
    }

    /// Older CLIs send `summary` with no display-line fields, either as the structured object
    /// or as the short-lived string. Ignoring those names must still produce a decodable page;
    /// the title falls back to project and save time.
    func testOlderSessionsPayloadsStillDecode() throws {
        let json = #"""
        {"schema_version":2,"sessions":[
          {"role":"chair","consistency":"current","summary_status":"structured",
           "summary":{"now":"Old CLI","next":null,"risk":null,"updated_at":"2026-11-02T01:58:26Z","needs_you":[]},
           "record":{"host_session_id":"object-summary","project_id":null,"created_at":null,
           "last_saved_at":null,"summary":{"now":"Old CLI","next":null,"risk":null,
           "updated_at":"2026-11-02T01:58:26Z","needs_you":[],"revision":1,"schema_version":1}}},
          {"role":"chair","consistency":"current","summary":"Old CLI display line","summary_status":"title",
           "record":{"host_session_id":"string-summary","project_id":null,"created_at":null,
           "last_saved_at":null,"summary":null}}],
          "next_cursor":null,"truncated":false,"scan_truncated":false}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let page = try decoder.decode(Sessions.self, from: Data(json.utf8))
        XCTAssertEqual(page.sessions.count, 2)
        XCTAssertTrue(page.sessions.allSatisfy { $0.summaryLine == nil && $0.summarySource == nil })
        XCTAssertEqual(page.sessions.first?.record.summary?.now, "Old CLI")
    }

    func testTreeSearchOutputAndQuotaDecode() async throws {
        let (console, replay) = client([(["console", "tree"], "console-tree", 0), (["console", "search"], "console-search", 0),
                                        (["output"], "output-result", 0), (["quota", "show"], "quota-show", 0),
                                        (["quota", "history"], "quota-history", 0)])
        let tree = try await console.tree()
        XCTAssertEqual(tree.nodes.map(\.depth), [0, 1, 1, 2, 0, 0])
        XCTAssertEqual(tree.nodes.first?.title, "implement-console-overview")
        XCTAssertEqual(tree.nodes.last?.title, "run-the-whole-test-suite · task--writer · attempt 2")
        XCTAssertEqual(tree.nodes.first?.children?.map(\.kind), ["dispatch", "dispatch"])
        let search = try await console.search("fixture", part: "result")
        XCTAssertEqual(search.matches.count, 2)
        XCTAssertEqual(search.matches.first?.part, "result")
        let output = try await console.output("c1c1", part: "result")
        XCTAssertTrue(output.text?.hasPrefix("The overview now joins") == true)
        let quota = try await console.quota()
        XCTAssertEqual(quota.accounts.map(\.account.label), ["Anese", "default", "Account 01"])
        XCTAssertEqual(quota.accounts.first?.snapshot?.windows.first?.usedBasisPoints, 8400)
        let history = try await console.quotaHistory()
        XCTAssertEqual(history.readings.count, 72)
        let calls = replay.recorded
        XCTAssertEqual(calls[0], ["console", "tree", "--detail", "--max-nodes", "100", "--json"])
        XCTAssertEqual(calls[1], ["console", "search", "--part", "result", "--limit", "50", "--", "fixture", "--json"])
        XCTAssertEqual(calls[2], ["output", "c1c1", "--part", "result", "--max-bytes", "16384", "--json"])
    }

    func testAReplyToAnotherCommandIsRefused() async throws {
        let (console, _) = client([(["console", "board"], "console-overview", 0)])
        do {
            _ = try await console.board()
            XCTFail("expected the envelope check to refuse a reply for another command")
        } catch let error as ClientError {
            XCTAssertEqual(error, .malformed("unexpected command envelope"))
        }
    }

    func testChangesBaselineHasEverythingUnknownAndLaterReadsCarrySince() async throws {
        let (console, replay) = client([(["console", "changes"], "console-changes", 0)])
        let first = try await console.changes()
        XCTAssertNil(first.changed.runs)
        XCTAssertNil(first.changed.inbox)
        XCTAssertEqual(first.generations.board, 14)
        XCTAssertEqual(first.generations.epoch, 6872134465275348)
        _ = try await console.changes(since: first.generations)
        XCTAssertEqual(replay.recorded[0], ["console", "changes", "--json"])
        XCTAssertEqual(replay.recorded[1][3],
                       #"{"epoch":6872134465275348,"inbox":14,"board":14,"chair":2,"quota":null,"configuration":null,"runs":null}"#)
    }

    func testChangesAfterARunWriteMoveBoardAndRuns() async throws {
        let (console, _) = client([(["console", "changes"], "console-changes-after-run-write", 0)])
        let changes = try await console.changes()
        XCTAssertEqual(changes.changed.runs, true)
        XCTAssertEqual(changes.changed.board, true)
        XCTAssertNil(changes.changed.chair)
        XCTAssertEqual(changes.generations.runs, 101)
    }
}
