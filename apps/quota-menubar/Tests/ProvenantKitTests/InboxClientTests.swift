import Foundation
import XCTest
@testable import ProvenantKit

/// A runner that replays recorded CLI output by argument prefix and records every call.
final class Replay: @unchecked Sendable {
    private let lock = NSLock()
    private(set) var calls: [[String]] = []
    private var replies: [(prefix: [String], file: String, status: Int32)]
    init(_ replies: [(prefix: [String], file: String, status: Int32)]) { self.replies = replies }

    static func fixture(_ name: String) throws -> Data {
        try Data(contentsOf: URL(fileURLWithPath: #filePath).deletingLastPathComponent().appending(path: "Fixtures/\(name).json"))
    }

    var client: CLIClient {
        CLIClient { [self] arguments, _ in
            lock.withLock { calls.append(arguments) }
            let hit = lock.withLock { replies.first { arguments.starts(with: $0.prefix) } }
            guard let hit else { throw ClientError.launch("no recorded reply for \(arguments)") }
            return CLI.Output(status: hit.status, signalled: false, timedOut: false, truncated: false,
                              out: try Self.fixture(hit.file), err: "")
        }
    }
    var recorded: [[String]] { lock.withLock { calls } }
}

final class InboxClientTests: XCTestCase {
    func testListFollowsTheCursorAndKeepsKindBlockingAndSession() async throws {
        let replay = Replay([(["inbox", "--state", "open", "--limit", "50", "--cursor", "CUR1"], "inbox-page2", 0),
                             (["inbox", "--state", "open"], "inbox-page1", 0)])
        let page = try await InboxClient(cli: replay.client).list()
        XCTAssertEqual(page.items.map(\.request.title), ["Pick the storage layer", "Assume UTC timestamps", "Which name?"])
        XCTAssertEqual(page.items.map(\.request.kind), ["decision", "assumption", "question"])
        XCTAssertEqual(page.items.map(\.request.blocking), [true, false, false])
        XCTAssertEqual(page.items[0].chairSessionId, "3f9c2a10d4e84b7f")
        XCTAssertNil(page.nextCursor)
        XCTAssertEqual(replay.recorded.count, 2)
        XCTAssertTrue(replay.recorded.allSatisfy { $0.last == "--json" })
    }

    func testPageLimitReportsRemainingPages() async throws {
        let replay = Replay([(["inbox"], "inbox-page1", 0)])
        let page = try await InboxClient(cli: replay.client).list(maxPages: 1)
        XCTAssertEqual(page.items.count, 2)
        XCTAssertEqual(page.nextCursor, "CUR1")
        XCTAssertTrue(page.truncated)
    }

    func testAnswerUsesExpectRevisionAndConsoleChannelAndShowsDeliveryAsReturned() async throws {
        let replay = Replay([(["answer"], "answer-saved", 0)])
        let outcome = try await InboxClient(cli: replay.client)
            .answer("1", action: .choose, option: "sqlite", expectRevision: 3)
        guard case .saved(let receipt) = outcome else { return XCTFail("expected a saved answer") }
        XCTAssertEqual(receipt.delivery?.state, "queued")
        XCTAssertEqual(receipt.state, "answered")
        let call = try XCTUnwrap(replay.recorded.first)
        XCTAssertEqual(call, ["answer", "1", "--action", "choose", "--expect-revision", "3", "--channel", "console", "--option", "sqlite", "--json"])
    }

    func testStaleRevisionReadsTheCurrentStateAndWritesNothing() async throws {
        let replay = Replay([(["answer"], "answer-conflict", 1), (["inbox", "1"], "inbox-item-stale", 0)])
        let outcome = try await InboxClient(cli: replay.client).answer("1", action: .confirm, expectRevision: 3)
        guard case .stale(let current) = outcome else { return XCTFail("expected a stale outcome") }
        XCTAssertEqual(current.request.revision, 5)
        XCTAssertEqual(current.request.state, "superseded")
        XCTAssertEqual(replay.recorded.filter { $0.first == "answer" }.count, 1)
    }

    func testItemCarriesAnswerAndAcknowledgement() async throws {
        let replay = Replay([(["inbox", "1"], "inbox-item-answered", 0)])
        let item = try await InboxClient(cli: replay.client).item("1")
        XCTAssertEqual(item.answer?.channel, "console")
        XCTAssertEqual(item.delivery?.state, "acknowledged")
        XCTAssertEqual(item.delivery?.acknowledgedAt, "2026-10-30T01:06:09Z")
    }

    func testErrorEnvelopeBecomesTypedError() async throws {
        let replay = Replay([(["answer"], "answer-conflict", 1)])
        do {
            _ = try await replay.client.call(["answer"], as: InboxReceipt.self)
            XCTFail("expected an error")
        } catch let error as ClientError {
            XCTAssertEqual(error.code, "conflict")
        }
    }

    func testScheduledCallPreservesErrorsAndValidatesEnvelope() throws {
        func output(_ json: String, status: Int32 = 1) -> CLI.Output {
            CLI.Output(status: status, signalled: false, timedOut: false, truncated: false, out: Data(json.utf8), err: "fallback")
        }
        let failure = output(#"{"schema_version":1,"command":"console overview","status":"error","error":{"code":"broken","message":"Broken.","repair":"Repair it."}}"#)
        XCTAssertThrowsError(try CLIClient.decode(failure, command: "console overview", as: InboxReceipt.self)) { error in
            XCTAssertEqual((error as? ClientError)?.message, "Broken. Repair it.")
        }
        XCTAssertThrowsError(try CLIClient.decode(failure, command: "quota show", as: InboxReceipt.self)) { error in
            guard case .malformed = error as? ClientError else { return XCTFail("expected malformed envelope") }
        }
    }

    func testUnreadableOutputIsTypedAndBounded() async {
        let garbage = CLIClient { _, _ in CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false, out: Data("nope".utf8), err: "") }
        let big = CLIClient { _, _ in CLI.Output(status: 0, signalled: false, timedOut: false, truncated: true, out: Data(), err: "") }
        let failed = CLIClient { _, _ in CLI.Output(status: 2, signalled: false, timedOut: false, truncated: false, out: Data(), err: "boom\n") }
        for (client, expected) in [(garbage, "malformed"), (big, "outputTooLarge"), (failed, "exited")] {
            do { _ = try await client.call(["inbox"], as: InboxReceipt.self); XCTFail("expected an error") } catch let error as ClientError {
                XCTAssertTrue("\(error)".hasPrefix(expected), "\(error)")
            } catch { XCTFail("\(error)") }
        }
    }

    /// A runner that signals once it has the arguments, then holds the call open until `gate` is signalled.
    final class Slow: @unchecked Sendable {
        let started = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var arguments: [String] = []
        func run(_ arguments: [String]) throws -> CLI.Output {
            lock.withLock { self.arguments = arguments }
            started.signal()
            gate.wait()
            return CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false,
                              out: try Replay.fixture("answer-saved"), err: "")
        }
        var replyPath: String? {
            lock.withLock { arguments.firstIndex(of: "--text-file").map { arguments[$0 + 1] } }
        }
        /// Waits on a dispatch queue, since a blocking semaphore wait is unavailable in async code.
        func waitUntilStarted() async -> DispatchTimeoutResult {
            await withCheckedContinuation { continuation in
                DispatchQueue.global().async { continuation.resume(returning: self.started.wait(timeout: .now() + 5)) }
            }
        }
    }

    func testQuittingDuringASlowAnswerRemovesTheReplyFileWithinTheGrace() async throws {
        let slow = Slow(), replies = ReplyFiles()
        defer { slow.gate.signal() }
        let client = InboxClient(cli: CLIClient { arguments, _ in try slow.run(arguments) }, replies: replies)
        let answer = Task { try await client.answer("1", action: .reply, text: "a private reply", expectRevision: 3) }
        let started = await slow.waitUntilStarted()
        XCTAssertEqual(started, .success)
        let path = try XCTUnwrap(slow.replyPath)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        let begin = Date()
        replies.finish(within: .milliseconds(200))
        XCTAssertLessThan(Date().timeIntervalSince(begin), 2)
        XCTAssertFalse(FileManager.default.fileExists(atPath: path))
        slow.gate.signal()
        guard case .saved = try await answer.value else { return XCTFail("expected a saved answer") }
    }

    func testFinishWaitsForAnAnswerThatReturnsWithinTheGrace() async throws {
        let slow = Slow(), replies = ReplyFiles()
        defer { slow.gate.signal() }
        let client = InboxClient(cli: CLIClient { arguments, _ in try slow.run(arguments) }, replies: replies)
        let answer = Task { try await client.answer("1", action: .reply, text: "a private reply", expectRevision: 3) }
        _ = await slow.waitUntilStarted()
        let path = try XCTUnwrap(slow.replyPath)
        slow.gate.signal()
        replies.finish(within: .seconds(5))
        XCTAssertFalse(FileManager.default.fileExists(atPath: path))
        guard case .saved = try await answer.value else { return XCTFail("expected a saved answer") }
    }

    func testExecCapsOutput() throws {
        let run = try CLI.exec(URL(fileURLWithPath: "/usr/bin/yes"), [], path: nil, limit: 0.3, maxOutput: 1024)
        XCTAssertTrue(run.truncated)
        XCTAssertLessThanOrEqual(run.out.count, 1024)
    }
}
