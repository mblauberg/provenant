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
        XCTAssertEqual(lanes.map(\.title), ["Implement console overview", "Review the change set", "Run the whole test suite"])
        XCTAssertEqual(lanes.last?.state, "terminal")
        XCTAssertEqual(lanes.last?.attention, true)
        XCTAssertEqual(lanes.first?.projectLabel, "provenant")

        let (console, _) = client([(["console", "overview"], "console-overview", 0)])
        let overview = try await console.overview()
        XCTAssertNil(overview.counts.running)
        XCTAssertNil(overview.counts.attention)
        XCTAssertEqual(overview.counts.needsYou, 1)
        XCTAssertEqual(overview.needsYou.map(\.title), ["Review console"])
        XCTAssertEqual(overview.needsYou.first?.projectLabel, "project")
        XCTAssertEqual(overview.coverage.warnings, ["Runs: unavailable", "Nested runs: unavailable"])
    }

    func testBoardDecodesLanesAndPassesPagingArguments() async throws {
        let (console, replay) = client([(["console", "board"], "console-board", 0)])
        let board = try await console.board(all: true, limit: 25, cursor: "C1")
        XCTAssertEqual(board.lanes.count, 1)
        let lane = try XCTUnwrap(board.lanes.first)
        XCTAssertEqual(lane.title, "Board fixture")
        XCTAssertEqual(lane.metadata.attempt.state.state, "queued")
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
        XCTAssertEqual(sessions.first?.summary?.now, "Implementing console")
        XCTAssertEqual(sessions.first?.id, "console-chair")

        let (bare, _) = client([(["console", "sessions"], "console-sessions-without-summary", 0)])
        let none = try await bare.sessions().sessions
        XCTAssertFalse(none.isEmpty)
        XCTAssertTrue(none.allSatisfy { $0.summary == nil })
    }

    func testTreeSearchOutputAndQuotaDecode() async throws {
        let (console, replay) = client([(["console", "tree"], "console-tree", 0), (["console", "search"], "console-search", 0),
                                        (["output"], "output-result", 0), (["quota", "show"], "quota-show", 0),
                                        (["quota", "history"], "quota-history", 0)])
        let tree = try await console.tree()
        XCTAssertEqual(tree.nodes.map(\.depth), [0, 1, 1, 2, 0, 0])
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
