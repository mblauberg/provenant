import Foundation
import XCTest
import ProvenantKit
import ProvenantDesign
@testable import ProvenantConsole

import ProvenantTestSupport

private extension Replay {
    var inbox: InboxClient { InboxClient(cli: client) }
}

@MainActor
final class InboxModelTests: XCTestCase {
    func testRefreshListsRequestsAndKeepsThemWhenARefreshFails() async {
        let replay = Replay([(["inbox"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.inbox)
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.request.title), ["Which name?"])
        XCTAssertEqual(model.selection, model.requests.first?.id)
        XCTAssertNotNil(model.lastRefreshed)
        replay.set([])
        await model.refresh()
        XCTAssertEqual(model.requests.count, 1)
        XCTAssertNotNil(model.refreshError)
    }

    func testStaleAnswerShowsCurrentStateAndSendsNothingElse() async throws {
        let replay = Replay([(["answer"], "answer-conflict", 1), (["inbox", "--all-projects", String(repeating: "1", count: 32)], "inbox-item-stale", 0),
                             (["inbox", "--all-projects", "--state"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.inbox)
        await model.refresh()
        let page = try Replay.page1Request()
        await model.answer(page, action: .choose, option: "sqlite")
        let result = try XCTUnwrap(model.results[page.id])
        XCTAssertEqual(result.kind, .stale)
        XCTAssertEqual(result.item?.request.revision, 5)
        XCTAssertTrue(result.message.contains("Nothing was sent"))
        let answers = replay.recorded.filter { $0.first == "answer" }
        XCTAssertEqual(answers.count, 1)
        XCTAssertTrue(answers[0].contains("--expect-revision") && answers[0].contains("3") && answers[0].contains("console"))
    }

    func testSavedAnswerShowsDeliveryAsReturned() async throws {
        let replay = Replay([(["answer"], "answer-saved", 0), (["inbox", "--all-projects", String(repeating: "1", count: 32)], "inbox-item-answered", 0),
                             (["inbox", "--all-projects", "--state"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.inbox)
        let request = try Replay.page1Request()
        await model.answer(request, action: .choose, option: "sqlite")
        let result = try XCTUnwrap(model.results[request.id])
        XCTAssertEqual(result.kind, .saved)
        XCTAssertTrue(result.message.contains("Delivery queued"), result.message)
        XCTAssertEqual(result.item?.delivery?.state, "acknowledged")
        XCTAssertEqual(InboxModel.deliveryLine(try XCTUnwrap(result.item?.delivery), now: Stamp.parse("2026-10-30T01:16:09Z")!),
                       "Delivery acknowledged by fabric mail, 1 attempt, acknowledged \(Stamp.relative("2026-10-30T01:06:09Z", now: Stamp.parse("2026-10-30T01:16:09Z")!)).")
    }

    func testConfirmationAndDeliverySurviveAFailedItemReadAndARefresh() async throws {
        let replay = Replay([(["inbox", "--all-projects", "--state", "open", "--limit", "50", "--cursor", "CUR1"], "inbox-page2", 0),
                                    (["inbox", "--all-projects", "--state"], "inbox-page1", 0)])
        let model = InboxModel(client: replay.inbox)
        await model.refresh()
        let request = try XCTUnwrap(model.requests.first)
        XCTAssertEqual(model.selection, request.id)
        // The answer is saved, the read-back fails, and the list no longer holds the request.
        replay.set([(["answer"], "answer-saved", 0), (["inbox", "--all-projects", request.id], "answer-conflict", 1),
                    (["inbox", "--all-projects", "--state"], "inbox-page2", 0)])
        await model.answer(request, action: .choose, option: "sqlite")
        let result = try XCTUnwrap(model.results[request.id])
        XCTAssertEqual(result.kind, .saved)
        XCTAssertNil(result.item)
        XCTAssertTrue(result.message.contains("Delivery queued"), result.message)
        XCTAssertFalse(model.requests.contains { $0.id == request.id })
        XCTAssertEqual(model.selection, request.id)
        XCTAssertEqual(model.selected?.id, request.id)
        XCTAssertEqual(model.selected?.request.title, "Pick the storage layer")
    }

    func testFiltersAndProjectGroupsCountTheOpenRequests() async {
        let model = await InboxModel.consoleFixture()
        XCTAssertEqual(model.requests.count, 5)
        XCTAssertEqual(model.count(for: .all), 5)
        XCTAssertEqual(model.count(for: .blocking), 3)
        XCTAssertEqual(model.count(for: .decisions), 2)
        XCTAssertEqual(model.count(for: .questions), 2)
        XCTAssertEqual(model.count(for: .assumptions), 1)

        XCTAssertEqual(model.projectGroups.map(\.name), ["Alpha", "Beta", "No project"])
        XCTAssertEqual(model.projectGroups.map(\.count), [2, 2, 1])

        XCTAssertEqual(model.selection, model.requests.first?.id)

        model.show(.filter(.blocking))
        XCTAssertEqual(model.visibleRequests.map(\.request.title),
                       ["Pick the storage layer", "Choose a release day", "Should the queue retry?"])
        XCTAssertEqual(model.selection, model.visibleRequests.first?.id)

        model.show(.filter(.decisions))
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Pick the storage layer", "Choose a release day"])

        model.show(.filter(.questions))
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Which name?", "Should the queue retry?"])

        model.show(.filter(.assumptions))
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Assume UTC timestamps"])

        let alpha = model.projectGroups.first { $0.name == "Alpha" }
        XCTAssertNotNil(alpha)
        model.show(.project(alpha?.id ?? ""))
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Pick the storage layer", "Which name?"])
        XCTAssertEqual(model.selection, model.visibleRequests.first?.id)
    }

    private static let peer = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"

    private func peerModel(_ first: String = "inbox-peers-complete") async -> (InboxModel, Replay, [Date]) {
        let replay = Replay([(["inbox"], first, 0)])
        let clock = Clock()
        let model = InboxModel(client: replay.inbox, now: { clock.tick() })
        await model.refresh()
        return (model, replay, clock.times)
    }

    func testProjectsWithTheSameIDOnDifferentHostsGroupSeparately() async {
        let (model, _, _) = await peerModel()
        XCTAssertEqual(model.requests.count, 3)
        XCTAssertEqual(model.projectGroups.map(\.name), ["alpha", "alpha on studio"])
        XCTAssertEqual(model.projectGroups.map(\.count), [1, 2])
        XCTAssertEqual(Set(model.projectGroups.map(\.id)).count, 2)
        let studio = model.projectGroups.first { $0.name == "alpha on studio" }
        model.show(.project(studio?.id ?? ""))
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Peer decision", "Peer question"])
        XCTAssertEqual(model.visibleRequests.first?.projectPrefix, "alpha on studio · ")
        model.searchText = "studio"
        XCTAssertEqual(model.visibleRequests.count, 2)
        // Complete peers add no coverage line; an unsupported one says to update it.
        XCTAssertEqual(model.peerNotes.map(\.id), ["f".repeated(32)])
        XCTAssertEqual(model.peerNotes.first?.text, "Update laptop: it is too old to list requests here.")
    }

    func testAnUnreachablePeerKeepsItsLastRowsStaleAndAnswersStillGoToTheOwner() async throws {
        let (model, replay, _) = await peerModel()
        let before = model.requests.filter { $0.peerHost != nil }
        XCTAssertEqual(before.count, 2)
        XCTAssertTrue(model.staleSince.isEmpty)

        replay.set([(["inbox"], "inbox-peers-unreachable", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.id.first), ["1", "5", "6"])
        XCTAssertNotNil(model.staleSince(for: before[0]))
        XCTAssertNil(model.staleSince(for: model.requests[0]))
        let note = try XCTUnwrap(model.peerNotes.first { $0.id == Self.peer })
        XCTAssertTrue(note.text.hasPrefix("studio is unreachable (timed out). Showing 2 from its last read, stale since"), note.text)
        // The peer is still unreachable on the next refresh: the same rows stay, with the same read time.
        let since = model.staleSince[Self.peer]
        await model.refresh()
        XCTAssertEqual(model.requests.count, 3)
        XCTAssertEqual(model.staleSince[Self.peer], since)

        replay.set([(["answer"], "answer-owner-unavailable", 1), (["inbox"], "inbox-peers-unreachable", 0)])
        await model.answer(before[0], action: .reply, text: "go")
        let call = try XCTUnwrap(replay.recorded.first { $0.first == "answer" })
        XCTAssertTrue(call.starts(with: ["answer", "--all-projects", before[0].id, "--host", Self.peer, "--project-key", "peer/key"]), "\(call)")
        let result = try XCTUnwrap(model.results[before[0].id])
        XCTAssertEqual(result.kind, .failed)
        XCTAssertTrue(result.message.contains("not reachable"), result.message)
        XCTAssertEqual(model.requests.count, 3)
    }

    func testACompleteOrUnsupportedPeerDropsItsKeptRows() async {
        let (model, replay, _) = await peerModel()
        replay.set([(["inbox"], "inbox-peers-unreachable", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.count, 3)

        replay.set([(["inbox"], "inbox-peers-complete", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.count, 3)
        XCTAssertTrue(model.staleSince.isEmpty)

        replay.set([(["inbox"], "inbox-peers-unreachable", 0)])
        await model.refresh()
        XCTAssertFalse(model.staleSince.isEmpty)
        replay.set([(["inbox"], "inbox-peers-unsupported", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.id.first), ["1"])
        XCTAssertTrue(model.staleSince.isEmpty)
        XCTAssertEqual(model.peerNotes.first?.text, "Update studio: it is too old to list requests here.")

        // A peer that vanished from the configuration is dropped too.
        replay.set([(["inbox"], "inbox-peers-complete", 0)])
        await model.refresh()
        replay.set([(["inbox"], "inbox-peers-dropped", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.id.first), ["1"])
        XCTAssertTrue(model.peerNotes.isEmpty)
    }

    func testPartialWithoutAReasonShowsItsRowsAndPartialWithAReasonKeepsTheLastOnes() async throws {
        let (model, replay, _) = await peerModel()
        XCTAssertEqual(model.requests.count, 3)

        // Gaps: the page is fresh, so it replaces the old rows, and the note says more exist.
        replay.set([(["inbox"], "inbox-peers-gaps", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.id.first), ["1", "5"])
        XCTAssertTrue(model.staleSince.isEmpty)
        XCTAssertEqual(model.peerNotes.first?.text, "1+ requests on studio; more are not loaded; 1 row from studio could not be used.")

        // No page with a reason: treated as unreachable, so the last good rows stay, stale.
        replay.set([(["inbox"], "inbox-peers-nopage", 0)])
        await model.refresh()
        XCTAssertEqual(model.requests.map(\.id.first), ["1", "5"])
        XCTAssertNotNil(model.staleSince[Self.peer])
        let note = try XCTUnwrap(model.peerNotes.first?.text)
        XCTAssertTrue(note.hasPrefix("studio gave no requests: it knows none of this Mac’s projects. Showing 1 from its last read"), note)

        // Without prior rows there is nothing to keep.
        let (fresh, _, _) = await peerModel("inbox-peers-nopage")
        XCTAssertEqual(fresh.requests.count, 1)
        XCTAssertTrue(fresh.staleSince.isEmpty)
        XCTAssertTrue(fresh.peerNotes.first?.text.hasSuffix("Its requests are not shown.") == true)
    }

    func testAnUnknownPeerStateWithRowsShowsThemWithANote() async {
        let (model, _, _) = await peerModel("inbox-peers-future")
        XCTAssertEqual(model.requests.count, 2)
        XCTAssertEqual(model.peerNotes.first?.text, "studio: coverage is degraded.")
    }

    func testAnOldCLIListingHasNoPeerNotes() async {
        let model = await InboxModel.consoleFixture()
        XCTAssertTrue(model.peerNotes.isEmpty)
        XCTAssertTrue(model.staleSince.isEmpty)
    }

    func testSearchNarrowsTheCurrentScopeWithoutChangingTheCounts() async {
        let model = await InboxModel.consoleFixture()
        let counts = InboxFilter.allCases.map { model.count(for: $0) }

        model.searchText = "storage"
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Pick the storage layer"])

        model.searchText = "beta"
        XCTAssertEqual(model.visibleRequests.map(\.request.title),
                       ["Assume UTC timestamps", "Choose a release day"])

        model.show(.filter(.questions))
        XCTAssertTrue(model.visibleRequests.isEmpty)
        XCTAssertNil(model.selection)

        model.searchText = ""
        XCTAssertEqual(model.visibleRequests.map(\.request.title), ["Which name?", "Should the queue retry?"])
        XCTAssertEqual(model.selection, model.visibleRequests.first?.id)
        XCTAssertEqual(InboxFilter.allCases.map { model.count(for: $0) }, counts)
    }

    func testKeyboardSelectionMovesThroughTheVisibleListAndStopsAtEachEnd() async {
        let model = await InboxModel.consoleFixture()
        let visible = model.visibleRequests
        XCTAssertEqual(model.selection, visible.first?.id)

        model.selectNext()
        XCTAssertEqual(model.selection, visible[1].id)
        model.selectNext()
        XCTAssertEqual(model.selection, visible[2].id)
        model.selectPrevious()
        XCTAssertEqual(model.selection, visible[1].id)

        for _ in 0..<10 { model.selectNext() }
        XCTAssertEqual(model.selection, visible.last?.id)
        for _ in 0..<10 { model.selectPrevious() }
        XCTAssertEqual(model.selection, visible.first?.id)

        model.show(.filter(.assumptions))
        XCTAssertEqual(model.selection, "33333333333333333333333333333333")
        model.show(.filter(.all))
        XCTAssertEqual(model.selection, "33333333333333333333333333333333")
    }

    func testNumberKeysChooseAnOptionAndAnUnknownNumberDoesNothing() async throws {
        let model = await InboxModel.consoleFixture()
        let request = try XCTUnwrap(model.requests.first { $0.request.options.count == 2 })
        XCTAssertEqual(model.draft(for: request).optionID, "sqlite")

        XCTAssertTrue(model.chooseOption(number: 1, for: request))
        XCTAssertEqual(model.draft(for: request).optionID, "sqlite")
        XCTAssertTrue(model.chooseOption(number: 2, for: request))
        XCTAssertEqual(model.draft(for: request).optionID, "files")
        XCTAssertFalse(model.chooseOption(number: 9, for: request))
        XCTAssertEqual(model.draft(for: request).optionID, "files")
    }

    func testEveryKindOffersTheFullActionSetWithAndWithoutOptions() throws {
        func draft(kind: String, options: Bool, proceeding: Bool = false) throws -> HumanRequest.Draft {
            let opts = options ? #"[{"id":"a","label":"A","description":"a"}]"# : "[]"
            let json = """
            {"kind":"\(kind)","title":"t","body":"b","blocking":true,"options":\(opts),
             "recommended_option_id":null,"proceeding_with":\(proceeding ? #""x""# : "null"),"deadline":null}
            """
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(HumanRequest.Draft.self, from: Data(json.utf8))
        }
        let cases: [(String, Bool, Bool, [AnswerAction], AnswerAction)] = [
            ("decision", false, false, [.reply], .reply),
            ("decision", true, false, [.choose, .reply], .choose),
            ("assumption", false, false, [.confirm, .override, .reply], .confirm),
            ("assumption", true, false, [.choose, .confirm, .override, .reply], .choose),
            ("Assumption", false, false, [.confirm, .override, .reply], .confirm),
            ("question", false, true, [.confirm, .override, .reply], .confirm),
        ]
        for (kind, options, proceeding, actions, primary) in cases {
            let d = try draft(kind: kind, options: options, proceeding: proceeding)
            XCTAssertEqual(InboxActions.available(for: d), actions, "\(kind) options=\(options)")
            XCTAssertEqual(InboxActions.primary(for: d), primary, "\(kind) options=\(options)")
        }
    }

    func testReplyDraftsStayWithTheirRequest() async throws {
        let model = await InboxModel.consoleFixture()
        let first = try XCTUnwrap(model.requests.first)
        let second = try XCTUnwrap(model.requests.dropFirst().first)

        model.setReply("Use the smaller file.", for: first)
        XCTAssertEqual(model.reply(for: first), "Use the smaller file.")
        XCTAssertEqual(model.reply(for: second), "")

        model.setReply("Call it harbour.", for: second)
        XCTAssertEqual(model.reply(for: first), "Use the smaller file.")
        XCTAssertEqual(model.reply(for: second), "Call it harbour.")
    }
}

private extension Replay {
    /// The first request of the recorded first page (revision 3).
    static func page1Request() throws -> HumanRequest {
                struct Page: Decodable { let items: [HumanRequest] }
        let envelope = try Envelope<Page>.decode(Replay.fixture("inbox-page1"))
        return try XCTUnwrap(envelope.data?.items.first)
    }
}

@MainActor
private extension InboxModel {
    /// A model replayed from `inbox-console.json` and refreshed once.
    static func consoleFixture() async -> InboxModel {
        let replay = Replay([(["inbox"], "inbox-console", 0)])
        let model = InboxModel(client: replay.inbox)
        await model.refresh()
        return model
    }
}

final class InboxSelectionTests: XCTestCase {
    func testMoveStopsAtEachEndAndEmptyClears() {
        var s = InboxSelection(id: "b")
        s.move(by: 1, in: ["a", "b", "c"]); XCTAssertEqual(s.id, "c")
        s.move(by: 1, in: ["a", "b", "c"]); XCTAssertEqual(s.id, "c")
        s.move(by: -5, in: ["a", "b", "c"]); XCTAssertEqual(s.id, "a")
        s.move(by: 1, in: []); XCTAssertNil(s.id)
    }

    func testMoveWithNothingSelectedPicksAnEnd() {
        var s = InboxSelection()
        s.move(by: 1, in: ["a", "b"]); XCTAssertEqual(s.id, "a")
        s = InboxSelection(id: "gone")
        s.move(by: -1, in: ["a", "b"]); XCTAssertEqual(s.id, "b")
    }

    func testReconcileKeepsListedOrPinnedElseFirst() {
        var s = InboxSelection(id: "b")
        s.reconcile(visible: ["a", "b"], pinned: [], keepPinned: false); XCTAssertEqual(s.id, "b")
        s.reconcile(visible: ["a"], pinned: ["b"], keepPinned: true); XCTAssertEqual(s.id, "b")
        s.reconcile(visible: ["a"], pinned: ["b"], keepPinned: false); XCTAssertEqual(s.id, "a")
        s.reconcile(visible: [], pinned: [], keepPinned: false); XCTAssertNil(s.id)
    }

    func testRequestKindKeepsUnknownKinds() {
        XCTAssertEqual(RequestKind("Assumption"), .assumption)
        XCTAssertEqual(RequestKind("review"), .other("review"))
        XCTAssertEqual(RequestKind("review").title, "Review")
    }
}

private final class Clock: @unchecked Sendable {
    private let lock = NSLock()
    private var next = Date(timeIntervalSince1970: 1_800_000_000)
    private(set) var times: [Date] = []
    func tick() -> Date {
        lock.withLock {
            next = next.addingTimeInterval(600)
            times.append(next)
            return next
        }
    }
}

private extension String {
    func repeated(_ count: Int) -> String { String(repeating: self, count: count) }
}
