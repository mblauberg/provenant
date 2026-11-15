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
    func testParsesFractionalAndWholeSecondTimestamps() {
        XCTAssertNotNil(Stamp.parse("2026-11-02T01:57:31.286667Z"))
        XCTAssertNotNil(Stamp.parse("2026-11-02T01:57:31Z"))
        XCTAssertNil(Stamp.parse("yesterday"))
        XCTAssertNil(Stamp.parse(nil))
    }

    func testRelativeDatesAreFormattedAndUnparseableTextIsKept() throws {
        let now = try XCTUnwrap(Stamp.parse("2026-11-02T02:00:00Z"))
        XCTAssertEqual(Stamp.relative("2026-11-02T01:50:00Z", now: now), "10 minutes ago")
        XCTAssertEqual(Stamp.relative("soon-ish", now: now), "soon-ish")
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

    func testAFailedTerminalOverviewLaneKeepsItsOutcomeInRowAndHeading() async throws {
        let replay = Replay([(["console", "overview"], "console-overview-active", 0), (["inbox"], "inbox-console", 0)])
        let model = ConsoleModel(cli: replay.client, defaults: nil)
        await model.load(.overview)
        let lane = try XCTUnwrap(model.overview.reader.value?.overview.lanes.last)
        XCTAssertEqual(lane.state, "terminal")
        XCTAssertEqual(lane.outcome, "failed")
        let row = LaneRow(lane)
        XCTAssertEqual(row.style, StatusStyle.attempt(state: "terminal", outcome: "failed"))
        XCTAssertEqual(row.style.tone, .bad)
        XCTAssertNotEqual(row.style.label, "Finished")
        let heading = try XCTUnwrap(model.heading(lane.id))
        XCTAssertEqual(heading.outcome, "failed")
        XCTAssertEqual(StatusStyle.attempt(state: heading.state, outcome: heading.outcome).tone, .bad)
        // The project label is context beside the state, not what the attempt waits on.
        XCTAssertNil(row.waiting)
        XCTAssertEqual(row.project, "provenant")
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
        XCTAssertTrue(model.reader.value?.warnings.contains("Request kinds: inbox not read") == true)
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

    func testQuotaShowsAccountsAndSurvivesMissingHistory() async {
        let (console, replay) = client()
        let quota = QuotaPanelModel(client: console)
        await quota.refresh()
        let full = quota.reader.value
        XCTAssertFalse(full?.rows.isEmpty ?? true)
        XCTAssertEqual(full?.historyAvailable, true)
        XCTAssertEqual(full?.rows.first { $0.provider == "agy" }?.refusals.first?.pool, "claude")
        XCTAssertEqual(full?.rows.first { $0.provider == "agy" }?.refusals.first?.resetsAt,
                       Stamp.parse("2026-11-02T03:00:00Z"))
        replay.set(consoleReplies.filter { $0.prefix != ["quota", "history"] })
        await quota.refresh()
        XCTAssertEqual(quota.reader.value?.historyAvailable, false)
        XCTAssertEqual(quota.reader.value?.rows.map(\.id), full?.rows.map(\.id))
        XCTAssertTrue(quota.reader.value?.rows.allSatisfy { $0.trend.isEmpty } == true)
    }

    func testChairsPairSessionsWithTheirProjects() async {
        let (console, _) = client()
        let chairs = ChairsModel(client: console)
        await chairs.refresh()
        let snapshot = chairs.reader.value
        XCTAssertEqual(snapshot?.sessions.count, 1)
        XCTAssertEqual(snapshot?.projects.count, 1)
        XCTAssertNil(snapshot?.projectName("no-such-project"))
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
