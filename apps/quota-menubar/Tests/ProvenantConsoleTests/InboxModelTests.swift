import Foundation
import XCTest
import ProvenantKit
@testable import ProvenantConsole

/// Replays the recorded CLI output in `ProvenantKitTests/Fixtures`.
private final class FixtureReplay: @unchecked Sendable {
    private let lock = NSLock()
    private var calls: [[String]] = []
    private var replies: [(prefix: [String], file: String, status: Int32)]
    init(_ replies: [(prefix: [String], file: String, status: Int32)]) { self.replies = replies }

    var recorded: [[String]] { lock.withLock { calls } }
    func set(_ replies: [(prefix: [String], file: String, status: Int32)]) { lock.withLock { self.replies = replies } }

    var client: InboxClient {
        InboxClient(cli: CLIClient { [self] arguments, _ in
            let hit = lock.withLock { calls.append(arguments); return replies.first { arguments.starts(with: $0.prefix) } }
            guard let hit else { throw ClientError.launch("no recorded reply for \(arguments)") }
            let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .appending(path: "../ProvenantKitTests/Fixtures/\(hit.file).json").standardized
            return CLI.Output(status: hit.status, signalled: false, timedOut: false, truncated: false,
                              out: try Data(contentsOf: url), err: "")
        })
    }
}

@MainActor
final class InboxModelTests: XCTestCase {
    func testRefreshListsRequestsAndKeepsThemWhenARefreshFails() async {
        let replay = FixtureReplay([(["inbox"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.client)
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
        let replay = FixtureReplay([(["answer"], "answer-conflict", 1), (["inbox", "--all-projects", String(repeating: "1", count: 32)], "inbox-item-stale", 0),
                             (["inbox", "--all-projects", "--state"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.client)
        await model.refresh()
        let page = try FixtureReplay.page1Request()
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
        let replay = FixtureReplay([(["answer"], "answer-saved", 0), (["inbox", "--all-projects", String(repeating: "1", count: 32)], "inbox-item-answered", 0),
                             (["inbox", "--all-projects", "--state"], "inbox-page2", 0)])
        let model = InboxModel(client: replay.client)
        let request = try FixtureReplay.page1Request()
        await model.answer(request, action: .choose, option: "sqlite")
        let result = try XCTUnwrap(model.results[request.id])
        XCTAssertEqual(result.kind, .saved)
        XCTAssertTrue(result.message.contains("Delivery queued"), result.message)
        XCTAssertEqual(result.item?.delivery?.state, "acknowledged")
        XCTAssertEqual(InboxModel.deliveryLine(try XCTUnwrap(result.item?.delivery)),
                       "Delivery acknowledged by fabric mail, 1 attempt, acknowledged 2026-10-30T01:06:09Z.")
    }

    func testConfirmationAndDeliverySurviveAFailedItemReadAndARefresh() async throws {
        let replay = FixtureReplay([(["inbox", "--all-projects", "--state", "open", "--limit", "50", "--cursor", "CUR1"], "inbox-page2", 0),
                                    (["inbox", "--all-projects", "--state"], "inbox-page1", 0)])
        let model = InboxModel(client: replay.client)
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

    func testSearchNarrowsTheCurrentScopeWithoutChangingTheCounts() async {
        let model = await InboxModel.consoleFixture()
        let counts = InboxModel.Filter.allCases.map { model.count(for: $0) }

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
        XCTAssertEqual(InboxModel.Filter.allCases.map { model.count(for: $0) }, counts)
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

private extension FixtureReplay {
    /// The first request of the recorded first page (revision 3).
    static func page1Request() throws -> HumanRequest {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appending(path: "../ProvenantKitTests/Fixtures/inbox-page1.json").standardized
        struct Page: Decodable { let items: [HumanRequest] }
        let envelope = try Envelope<Page>.decode(Data(contentsOf: url))
        return try XCTUnwrap(envelope.data?.items.first)
    }
}

@MainActor
private extension InboxModel {
    /// A model replayed from `inbox-console.json` and refreshed once.
    static func consoleFixture() async -> InboxModel {
        let replay = FixtureReplay([(["inbox"], "inbox-console", 0)])
        let model = InboxModel(client: replay.client)
        await model.refresh()
        return model
    }
}
