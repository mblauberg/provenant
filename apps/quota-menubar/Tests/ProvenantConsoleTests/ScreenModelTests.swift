import AppKit
import Foundation
import XCTest
import ProvenantDesign
import ProvenantKit
import ProvenantTestSupport
@testable import ProvenantConsole

private func changed(_ json: String) throws -> ChangedScopes {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(ChangedScopes.self, from: Data(json.utf8))
}

private let consoleReplies: [Replay.Reply] = [
    (["console", "overview"], "console-overview", 0), (["console", "board"], "console-board", 0),
    (["console", "tree"], "console-tree", 0), (["console", "sessions"], "console-sessions", 0),
    (["console", "projects"], "console-projects", 0), (["console", "search"], "console-search", 0),
    (["console", "detail"], "console-detail", 0), (["output"], "output-result", 0),
    (["quota", "show"], "quota-show", 0), (["quota", "history"], "quota-history", 0),
    (["inbox"], "inbox-console", 0),
]

final class DestinationTests: XCTestCase {
    func testNumberedDestinationsMapToCommandOneThroughFive() {
        XCTAssertEqual(Destination.numbered, [.overview, .inbox, .runs, .quota, .chairs])
        XCTAssertEqual(Destination.numbered.compactMap(\.shortcut).map(String.init), ["1", "2", "3", "4", "5"])
        XCTAssertNil(Destination.search.shortcut)
    }

    func testSidebarItemsRoundTripTheirDestination() {
        for destination in Destination.allCases { XCTAssertEqual(SidebarItem(destination).destination, destination) }
        XCTAssertEqual(SidebarItem(.inbox), .inbox(.all))
    }

    func testSearchScopesMapToParts() {
        XCTAssertNil(SearchScope.requests.part)
        XCTAssertEqual(SearchScope.prompts.part, .prompt)
        XCTAssertEqual(SearchScope.results.part, .result)
    }
}

final class ConsoleDeepLinkTests: XCTestCase {
    func testParsesOneEncodedIdentifierForSupportedRoutes() throws {
        XCTAssertEqual(ConsoleDeepLink.parse(try XCTUnwrap(URL(string: "provenant-console://request/request%2Fchild"))),
                       .request("request/child"))
        XCTAssertEqual(ConsoleDeepLink.parse(try XCTUnwrap(URL(string: "provenant-console://attempt/attempt-1"))),
                       .attempt("attempt-1"))
    }

    func testRejectsUnknownOrAmbiguousRoutes() throws {
        for value in ["other-console://request/id", "provenant-console://unknown/id",
                      "provenant-console://request/", "provenant-console://request/id/extra",
                      "provenant-console://request/id?query=value"] {
            XCTAssertNil(ConsoleDeepLink.parse(try XCTUnwrap(URL(string: value))), value)
        }
    }
}

final class StampTests: XCTestCase {
    /// Relative-date strings are locale-dependent; pin English so the assertions hold everywhere.
    private let locale = Locale(identifier: "en_US_POSIX")

    func testParsesFractionalAndWholeSecondTimestamps() {
        XCTAssertNotNil(Stamp.parse("2026-11-02T01:57:31.286667Z"))
        XCTAssertNotNil(Stamp.parse("2026-11-02T01:57:31Z"))
        XCTAssertNil(Stamp.parse("yesterday"))
        XCTAssertNil(Stamp.parse(nil))
    }

    func testRelativeDatesAreFormattedAndUnparseableTextIsKept() throws {
        let now = try XCTUnwrap(Stamp.parse("2026-11-02T02:00:00Z"))
        XCTAssertEqual(Stamp.relative("2026-11-02T01:50:00Z", now: now, locale: locale), "10 minutes ago")
        XCTAssertEqual(Stamp.relative("soon-ish", now: now, locale: locale), "soon-ish")
    }

    func testPastTimestampFormatsAsPast() throws {
        let now = try XCTUnwrap(Stamp.parse("2026-11-22T09:00:00Z"))
        let sixDaysAgo = Stamp.relative("2026-11-16T03:41:05.964523Z", now: now, locale: locale)
        XCTAssertEqual(sixDaysAgo, "6 days ago")
        XCTAssertFalse(sixDaysAgo.contains("next"))
        XCTAssertFalse(sixDaysAgo.contains("in "))
        XCTAssertEqual(Stamp.relative("2026-11-15T09:00:00Z", now: now, locale: locale), "last week")
    }

    func testDurations() {
        XCTAssertEqual(Stamp.duration(milliseconds: 250), "250 ms")
        XCTAssertEqual(Stamp.duration(milliseconds: 1_500), "1.5 s")
    }
}

@MainActor
final class ConsoleModelTests: XCTestCase {
    private func model() -> (ConsoleModel, Replay) {
        let replay = Replay(consoleReplies)
        return (ConsoleModel(cli: replay.client, defaults: nil), replay)
    }

    func testUnknownOverviewLaneKeepsItsStateInRowAndHeading() async throws {
        let replay = Replay([(["console", "overview"], "console-overview-active", 0), (["inbox"], "inbox-console", 0)])
        let model = ConsoleModel(cli: replay.client, defaults: nil)
        await model.load(.overview)
        let lane = try XCTUnwrap(model.overview.reader.value?.overview.lanes.last)
        XCTAssertEqual(lane.state, "unknown")
        let row = LaneRow(lane)
        XCTAssertEqual(row.style, StatusStyle.attempt(state: "unknown"))
        XCTAssertEqual(row.style.tone, .neutral)
        let heading = try XCTUnwrap(model.heading(lane.id))
        XCTAssertEqual(StatusStyle.attempt(state: heading.state).tone, .neutral)
        // The project label is context beside the state, not what the attempt waits on.
        XCTAssertNil(row.waiting)
        XCTAssertEqual(row.project, "provenant")
    }

    func testFailedOverviewStateKeepsItsRedRowAndHeadingStyle() throws {
        let json = #"""
        {
          "counts": {"needs_you": 0, "running": 0, "attention": 0},
          "coverage": {"scope": "local", "inbox": "complete", "runs": "complete", "nested_runs": "unavailable"},
          "needs_you": [],
          "lanes": [{
            "project_id": "01010101010101010101010101010101",
            "project_label": "provenant",
            "attempt_id": "failed-attempt",
            "task_label": "Synthetic failure",
            "state": "failed",
            "attention": true,
            "key": {"attempt": 1, "graph": "synthetic-failure", "node": "task", "project": "01010101010101010101010101010101"}
          }]
        }
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(json.utf8))
        let lane = try XCTUnwrap(overview.lanes.first)
        let model = ConsoleModel(cli: Replay().client, defaults: nil)
        model.overview.reader.replace(OverviewSnapshot(overview: overview, needsYou: [], inboxJoined: true))

        let row = LaneRow(lane)
        XCTAssertEqual(row.style, StatusStyle.attempt(state: "failed"))
        XCTAssertEqual(row.style.tone, .bad)
        let heading = try XCTUnwrap(model.heading(lane.id))
        XCTAssertEqual(heading.state, "failed")
        XCTAssertEqual(StatusStyle.attempt(state: heading.state).tone, .bad)
    }

    func testLoadedAgesUseTheSameClockAsDisplayedAges() async throws {
        let clock = try XCTUnwrap(Stamp.parse("2026-11-22T09:00:00Z"))
        let replay = Replay(consoleReplies)
        let model = ConsoleModel(cli: replay.client, now: { clock }, defaults: nil)
        await model.load(.runs)
        let loaded = try XCTUnwrap(model.runs.board.loadedAt)
        XCTAssertEqual(loaded, clock)
        XCTAssertEqual(Stamp.relative(loaded, now: model.inbox.displayNow), "just now")
    }

    func testStartsOnTheOverviewWithEverythingStale() {
        let (model, _) = model()
        XCTAssertEqual(model.destination, .overview)
        XCTAssertEqual(model.stale, Set(Destination.allCases))
    }

    func testLoadingAScreenClearsItsStaleMark() async {
        let (model, _) = model()
        await model.load(.runs)
        XCTAssertFalse(model.stale.contains(.runs))
        XCTAssertTrue(model.stale.contains(.quota))
    }

    func testChangeScopesMapToTheViewsThatShowThem() async throws {
        let (model, _) = model()
        for destination in Destination.allCases { await model.load(destination) }
        XCTAssertTrue(model.stale.isEmpty)
        // Not on the screens being changed, so nothing is read here: the marks alone are the result.
        model.show(.chairs)
        await model.load(.chairs)
        model.apply(try changed(#"{"inbox":true,"board":false,"chair":false,"quota":false,"configuration":false,"runs":false}"#))
        XCTAssertEqual(model.stale, [.inbox, .overview])
        model.apply(try changed(#"{"inbox":false,"board":false,"chair":false,"quota":true,"configuration":false,"runs":false}"#))
        XCTAssertTrue(model.stale.contains(.quota))
        model.apply(try changed(#"{"inbox":false,"board":false,"chair":false,"quota":false,"configuration":false,"runs":true}"#))
        XCTAssertTrue(model.stale.isSuperset(of: [.runs, .overview, .search]))
    }

    func testNullScopesReloadEverythingAndTheShownViewReloads() async throws {
        let (model, replay) = model()
        for destination in Destination.allCases { await model.load(destination) }
        model.apply(try changed(#"{"inbox":null,"board":null,"chair":null,"quota":null,"configuration":null,"runs":null}"#))
        XCTAssertTrue(model.stale.isSuperset(of: [.overview, .inbox, .runs, .quota, .chairs]))
        _ = replay
    }

    func testSelectionIsRememberedExceptForSearch() {
        let suite = UserDefaults(suiteName: "console-model-tests-\(UUID().uuidString)")!
        let replay = Replay(consoleReplies)
        let first = ConsoleModel(cli: replay.client, defaults: suite)
        first.select(.quota)
        XCTAssertEqual(ConsoleModel(cli: replay.client, defaults: suite).destination, .quota)
        first.select(.search)
        XCTAssertEqual(ConsoleModel(cli: replay.client, defaults: suite).destination, .quota)
    }

    func testReturningToTheInboxKeepsTheChosenScope() {
        let (model, _) = model()
        model.select(.inbox(.filter(.blocking)))
        model.show(.runs)
        model.show(.inbox)
        XCTAssertEqual(model.item, .inbox(.filter(.blocking)))
    }

    func testOpeningARequestRefreshesAndSelectsItInTheInbox() async {
        let (model, _) = model()
        model.searchText = "an old filter"
        await model.openRequest("11111111111111111111111111111111")
        XCTAssertEqual(model.destination, .inbox)
        XCTAssertEqual(model.item, .inbox(.all))
        XCTAssertEqual(model.inbox.selection, "11111111111111111111111111111111")
        XCTAssertEqual(model.inbox.selected?.request.title, "Pick the storage layer")
    }

    func testOpeningAnAttemptSelectsRunsAndTheInspector() async {
        let (model, _) = model()
        await model.openAttempt("65656565656565656565656565656565")
        XCTAssertEqual(model.destination, .runs)
        XCTAssertTrue(model.inspectorShown)
        XCTAssertEqual(model.attempt.attemptID, "65656565656565656565656565656565")
    }

    func testRequestSearchStaysInTheInboxAndOtherScopesGoToResults() async {
        let (model, replay) = model()
        model.searchText = "storage"
        model.submitSearch()
        XCTAssertEqual(model.destination, .inbox)
        XCTAssertTrue(replay.recorded.filter { $0.first == "console" && $0.contains("search") }.isEmpty)

        model.searchScope = .results
        model.submitSearch()
        XCTAssertEqual(model.destination, .search)
        for _ in 0..<50 where model.search.reader.value == nil { await Task.yield(); try? await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(model.search.query, "storage")
        XCTAssertEqual(model.search.part, .result)
        XCTAssertNotNil(model.search.reader.value)
    }
}

@MainActor
final class ScreenModelTests: XCTestCase {
    private func client(_ replies: [Replay.Reply] = consoleReplies) -> (ConsoleClient, Replay) {
        let replay = Replay(replies)
        return (ConsoleClient(cli: replay.client), replay)
    }

    func testOverviewJoinsKindsFromTheInboxAndSortsBlockingFirst() async {
        let (console, replay) = client()
        let model = OverviewModel(console: console, inbox: InboxClient(cli: replay.client))
        await model.refresh()
        let snapshot = model.reader.value
        XCTAssertNotNil(snapshot)
        XCTAssertEqual(snapshot?.inboxJoined, true)
        let rows = snapshot?.needsYou ?? []
        let blocking = rows.map(\.blocking)
        XCTAssertEqual(blocking, blocking.sorted { $0 && !$1 })
    }

    func testOverviewSurvivesAnInboxFailureAndSaysKindsAreUnknown() async {
        let (console, replay) = client(Array(consoleReplies.filter { $0.prefix != ["inbox"] }))
        let model = OverviewModel(console: console, inbox: InboxClient(cli: replay.client))
        await model.refresh()
        XCTAssertNil(model.reader.error)
        XCTAssertEqual(model.reader.value?.inboxJoined, false)
        XCTAssertEqual(model.reader.value?.notices,
                       [.warning("Request kinds are unavailable: the inbox could not be read.")])
        XCTAssertTrue(model.reader.value?.needsYou.allSatisfy { $0.kind == nil } == true)
    }

    func testFailedRefreshKeepsTheLastGoodValueAndShowsTheError() async {
        let (console, replay) = client()
        let model = OverviewModel(console: console, inbox: InboxClient(cli: replay.client))
        await model.refresh()
        replay.set([])
        await model.refresh()
        XCTAssertNotNil(model.reader.value)
        XCTAssertNotNil(model.reader.error)
    }

    func testRunsPagesAppendWithoutDuplicatesAndStopAtTheLastPage() async {
        let (console, replay) = client()
        let runs = RunsModel(client: console, pageSize: 1)
        await runs.refresh()
        let first = runs.board.value
        XCTAssertEqual(first?.lanes.count, 1)
        XCTAssertNotNil(first?.nextCursor)
        replay.set([(["console", "board"], "console-board-empty", 0)])
        await runs.loadMore()
        XCTAssertEqual(runs.board.value?.lanes.count, 1)
        XCTAssertNil(runs.board.value?.nextCursor)
        let calls = replay.recorded.filter { $0.contains("--cursor") }
        XCTAssertEqual(calls.count, 1)
        await runs.loadMore()
        XCTAssertEqual(replay.recorded.filter { $0.contains("--cursor") }.count, 1)
    }

    func testRunsFailedPageKeepsThePagesAlreadyShown() async {
        let (console, replay) = client()
        let runs = RunsModel(client: console, pageSize: 1)
        await runs.refresh()
        replay.set([])
        await runs.loadMore()
        XCTAssertEqual(runs.board.value?.lanes.count, 1)
        XCTAssertNotNil(runs.moreError)
    }

    func testTreeIsReadOnlyWhenShownAndOrdersChildrenAfterTheirParent() async {
        let (console, replay) = client()
        let runs = RunsModel(client: console)
        await runs.refresh()
        XCTAssertTrue(replay.recorded.allSatisfy { !$0.contains("tree") })
        await runs.setMode(.tree)
        let nodes = runs.tree.value?.nodes ?? []
        XCTAssertFalse(nodes.isEmpty)
        let ordered = RunsModel.ordered(nodes)
        XCTAssertEqual(Set(ordered.map(\.id)), Set(nodes.map(\.id)))
        XCTAssertEqual(ordered.count, nodes.count)
        for (index, node) in ordered.enumerated() where node.depth > 0 {
            XCTAssertTrue(ordered[..<index].contains { $0.depth == node.depth - 1 }, "\(node.id) precedes its parent")
        }
    }

    func testIncludingFinishedRunsAsksForThem() async {
        let (console, replay) = client()
        let runs = RunsModel(client: console)
        runs.includeFinished = true
        await runs.refresh()
        XCTAssertTrue(replay.recorded.contains { $0.contains("--all") })
    }

    func testAttemptReadsDetailAndOutputAndChangingPartRereadsOutputOnly() async {
        let (console, replay) = client()
        let attempt = AttemptModel(client: console)
        await attempt.show("c1c1")
        XCTAssertNotNil(attempt.detail.value)
        XCTAssertNotNil(attempt.output.value)
        let details = replay.recorded.filter { $0.contains("detail") }.count
        await attempt.setPart(.prompt)
        XCTAssertEqual(replay.recorded.filter { $0.contains("detail") }.count, details)
        XCTAssertTrue(replay.recorded.last?.contains("prompt") == true, "\(String(describing: replay.recorded.last))")
    }

    func testAttemptSwitchingClearsTheOldDetail() async {
        let (console, replay) = client()
        let attempt = AttemptModel(client: console)
        await attempt.show("one")
        replay.set([])
        await attempt.show("two")
        XCTAssertNil(attempt.detail.value)
        XCTAssertNotNil(attempt.detail.error)
        XCTAssertEqual(attempt.attemptID, "two")
    }

    func testPartialQuotaSnapshotIsLowerBound() throws {
        let json = #"""
        {"generated_unix_seconds": 1, "accounts": [{"account": {"adapter": "codex", "account_key": "a", "label": "A"},
          "status": "available", "snapshot": {"windows": [{"pool": "primary", "used_basis_points": 2456}],
          "details": {"coverage": "partial"}}}]}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let show = try decoder.decode(QuotaShow.self, from: Data(json.utf8))
        let window = try XCTUnwrap(QuotaPanelModel.snapshot(show, history: nil).rows.first?.windows.first)
        XCTAssertTrue(window.lowerBound)
        XCTAssertEqual(window.percent, 24.56)
        XCTAssertEqual(window.displayPercent, 24)
        XCTAssertEqual(window.value, "≥24%")
    }

    func testConsoleHeadlineFollowsTheMenusAutoRuleForAMixedAccount() throws {
        let json = #"""
        {"generated_unix_seconds": 1795311983, "display": {"headline": "auto"},
         "accounts": [{"account": {"adapter": "agy", "account_key": "home", "label": "home"},
           "status": "available", "display": {"id": "agy-home", "hidden": false},
           "snapshot": {"windows": [
             {"pool": "gemini.weekly", "used_basis_points": 4968, "resets_unix_seconds": 1795856159, "window_seconds": 604800},
             {"pool": "gemini.5h", "used_basis_points": 0, "resets_unix_seconds": 1795329864, "window_seconds": 18000},
             {"pool": "claude.weekly", "used_basis_points": 10000, "resets_unix_seconds": 1795567522, "window_seconds": 604800},
             {"pool": "gemini.weekly:2", "used_basis_points": 646, "resets_unix_seconds": 1795855854, "window_seconds": 604800}]}}]}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let show = try decoder.decode(QuotaShow.self, from: Data(json.utf8))
        let row = try XCTUnwrap(QuotaPanelModel.snapshot(show, history: nil).rows.first)
        XCTAssertEqual(row.headlineWindow?.label, "Gemini weekly")
        XCTAssertEqual(row.headlineWindow?.value, "49%")
        XCTAssertEqual(row.windows.map(\.label),
                       ["Gemini weekly", "Gemini 5-hour", "Claude weekly", "Gemini weekly (2)"])
    }

    func testQuotaShowsAccountsAndSurvivesMissingHistory() async {
        let (console, replay) = client()
        let quota = QuotaPanelModel(client: console)
        await quota.refresh()
        let full = quota.reader.value
        XCTAssertFalse(full?.rows.isEmpty ?? true)
        XCTAssertEqual(full?.historyAvailable, true)
        // One human label per window, and the headline follows the account's chosen window.
        XCTAssertEqual(full?.rows.first { $0.provider == "claude" }?.headlineWindow?.label, "5-hour")
        XCTAssertEqual(full?.rows.first { $0.provider == "claude" }?.headlineWindow?.value, "84%")
        XCTAssertEqual(full?.rows.first { $0.provider == "claude" }?.windows.map(\.label), ["5-hour", "Weekly"])
        XCTAssertTrue(full?.rows.allSatisfy { row in row.windows.allSatisfy { !$0.label.contains("_") && !$0.label.contains(".") } } ?? false)
        XCTAssertEqual(full?.rows.first { $0.provider == "agy" }?.refusals.first?.pool, "claude")
        XCTAssertEqual(full?.rows.first { $0.provider == "agy" }?.refusals.first?.resetsAt,
                       Stamp.parse("2026-11-02T03:00:00Z"))
        replay.set(consoleReplies.filter { $0.prefix != ["quota", "history"] })
        await quota.refresh()
        XCTAssertEqual(quota.reader.value?.historyAvailable, false)
        XCTAssertEqual(quota.reader.value?.rows.map(\.id), full?.rows.map(\.id))
        XCTAssertTrue(quota.reader.value?.rows.allSatisfy { $0.trend.isEmpty } == true)
    }

    func testHistoryTrendKeepsAReadingWhoseWindowsHaveSinceReset() throws {
        let showJSON = #"""
        {"generated_unix_seconds": 1000, "accounts": [
          {"account": {"adapter": "claude", "account_key": "a", "label": "A"}, "status": "available",
           "snapshot": {"windows": [
             {"pool": "five_hour", "used_basis_points": 8400, "resets_unix_seconds": 1500, "window_seconds": 18000}]}}]}
        """#
        let historyJSON = #"""
        {"generated_unix_seconds": 1000, "count": 1, "truncated": false, "readings": [
          {"observed_unix_seconds": 500, "account_key": "a", "label": "A", "provider": "claude", "status": "available",
           "windows": [{"pool": "five_hour", "used_basis_points": 3000, "resets_unix_seconds": 900, "window_seconds": 18000}]}]}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let show = try decoder.decode(QuotaShow.self, from: Data(showJSON.utf8))
        let history = try decoder.decode(QuotaHistory.self, from: Data(historyJSON.utf8))
        let row = try XCTUnwrap(QuotaPanelModel.snapshot(show, history: history).rows.first)
        XCTAssertEqual(row.trend.count, 1)
        XCTAssertEqual(row.trend.first?.date, Date(timeIntervalSince1970: 500))
        XCTAssertEqual(row.trend.first?.percent, 30)
    }

    func testConsoleHeadlineIgnoresAnAdvisoryWindowLikeTheMenu() throws {
        let json = #"""
        {"generated_unix_seconds": 1, "accounts": [
          {"account": {"adapter": "claude", "account_key": "a", "label": "A"}, "status": "available",
           "snapshot": {"windows": [
             {"pool": "five_hour", "used_basis_points": 1000, "resets_unix_seconds": 1000, "window_seconds": 18000},
             {"pool": "weekly_scoped:Fable", "used_basis_points": 10000, "resets_unix_seconds": 1000, "window_seconds": 604800}]}}]}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let show = try decoder.decode(QuotaShow.self, from: Data(json.utf8))
        let row = try XCTUnwrap(QuotaPanelModel.snapshot(show, history: nil).rows.first)
        XCTAssertEqual(row.headlineWindow?.name, "five_hour")
        XCTAssertTrue(row.windows.first { $0.name == "weekly_scoped:Fable" }?.advisory == true)
    }

    func testTerminalOutcomeTonesSeparateFailuresFromOperatorActions() {
        for state in ["lost", "quarantined", "unsettled", "unclassified"] {
            XCTAssertEqual(StatusStyle.attempt(state: state).tone, .bad, state)
        }
        for state in ["auth_required", "permission_denied", "input_required", "uncommitted",
                      "model_unavailable", "rate_limited"] {
            XCTAssertEqual(StatusStyle.attempt(state: state).tone, .caution, state)
        }
    }

    func testChairsPairSessionsWithTheirProjects() async {
        let (console, _) = client()
        let chairs = ChairsModel(client: console)
        await chairs.refresh()
        let snapshot = chairs.reader.value
        XCTAssertEqual(snapshot?.sessions.count, 1)
        // Two saved records for one host session collapse to the later save.
        XCTAssertEqual(snapshot?.sessions.first?.summaryLine, "Implementing the console")
        XCTAssertEqual(snapshot?.sessions.first?.record.summary?.now, "Implementing the console")
        XCTAssertEqual(snapshot?.sessions.first?.record.lastSavedAt, "2026-11-02T02:18:00.000000Z")
        XCTAssertEqual(snapshot?.projects.count, 1)
        XCTAssertNil(snapshot?.projectName("no-such-project"))
    }

    func testRenderClockFollowsTheFixturesInsteadOfAStaleFixedDay() throws {
        let fixtures = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "ProvenantTestSupport/Fixtures")
        let clock = try XCTUnwrap(Render.fixtureClock(in: fixtures))
        let saved = try XCTUnwrap(Stamp.parse("2026-11-02T02:18:00Z"))
        XCTAssertGreaterThan(clock, saved)
        XCTAssertFalse(Stamp.relative("2026-11-02T02:18:00Z", now: clock).contains("in "))
    }

    func testChairRowShowsSavedContextWhenThereIsNoDerivedLine() async throws {
        let replies: [Replay.Reply] = [(["console", "sessions"], "console-sessions-without-summary", 0),
                                       (["console", "projects"], "console-projects", 0)]
        let (console, _) = client(replies)
        let chairs = ChairsModel(client: console)
        await chairs.refresh()
        let session = try XCTUnwrap(chairs.reader.value?.sessions.first)
        let project = chairs.reader.value?.projectName(session.record.projectId)
        let now = try XCTUnwrap(Stamp.parse("2026-11-22T09:00:00Z"))
        XCTAssertNil(session.record.summary)
        XCTAssertNil(session.summaryLine)
        XCTAssertEqual(session.summarySource, "unavailable")
        // An untouched checkpoint template has no derived line, so the title
        // carries project, session and save time instead.
        let title = ChairsModel.title(session, project: project, now: now)
        XCTAssertTrue(title.contains("project"), title)
        XCTAssertTrue(title.contains("saved"), title)
        XCTAssertFalse(title.contains("Console checkpoint"), title)
    }

    func testChairRowPrefersTheSummaryWhenPresent() async throws {
        let (console, _) = client()
        let chairs = ChairsModel(client: console)
        await chairs.refresh()
        let session = try XCTUnwrap(chairs.reader.value?.sessions.first)
        let now = try XCTUnwrap(Stamp.parse("2026-11-22T09:00:00Z"))
        XCTAssertEqual(ChairsModel.title(session, project: "project", now: now), "Implementing the console")
    }

    func testSearchRerunUsesTheLastQueryAndPart() async {
        let (console, replay) = client()
        let search = SearchModel(client: console)
        await search.rerun()
        XCTAssertTrue(replay.recorded.isEmpty)
        await search.run("fixture", part: .prompt)
        await search.rerun()
        let searches = replay.recorded.filter { $0.contains("search") }
        XCTAssertEqual(searches.count, 2)
        XCTAssertEqual(searches[0], searches[1])
        XCTAssertTrue(searches[0].contains("prompt"))
    }
}

@MainActor
final class WindowLifecycleTests: XCTestCase {
    func testClosingTheWindowKeepsTheAppAliveAndReopenMakesANewOne() throws {
        guard #available(macOS 26, *) else { throw XCTSkip("the console shell needs macOS 26") }
        _ = NSApplication.shared
        let delegate = AppDelegate(console: ConsoleModel(cli: Replay(consoleReplies).client, defaults: nil), watchesChanges: false)
        XCTAssertFalse(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApplication.shared))
        delegate.showWindow()
        let first = delegate.window
        XCTAssertNotNil(first)
        first?.close()
        // The delegate fronts the window itself, so it tells AppKit not to make another.
        XCTAssertFalse(delegate.applicationShouldHandleReopen(NSApplication.shared, hasVisibleWindows: false))
        XCTAssertNotNil(delegate.window)
        XCTAssertTrue(delegate.window?.isVisible == true)
        delegate.window?.close()
    }
}
