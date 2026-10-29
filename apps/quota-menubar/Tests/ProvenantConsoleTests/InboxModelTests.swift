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
        let replay = FixtureReplay([(["answer"], "answer-conflict", 1), (["inbox", String(repeating: "1", count: 32)], "inbox-item-stale", 0),
                             (["inbox", "--state"], "inbox-page2", 0)])
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
        let replay = FixtureReplay([(["answer"], "answer-saved", 0), (["inbox", String(repeating: "1", count: 32)], "inbox-item-answered", 0),
                             (["inbox", "--state"], "inbox-page2", 0)])
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
