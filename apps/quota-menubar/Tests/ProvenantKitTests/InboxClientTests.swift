import Foundation
import XCTest
@testable import ProvenantKit
import ProvenantTestSupport

final class InboxClientTests: XCTestCase {
    func testListFollowsTheCursorAndKeepsKindBlockingAndSession() async throws {
        let replay = Replay([(["inbox", "--all-projects", "--state", "open", "--limit", "50", "--cursor", "CUR1"], "inbox-page2", 0),
                             (["inbox", "--all-projects", "--state", "open"], "inbox-page1", 0)])
        let page = try await InboxClient(cli: replay.client).list()
        XCTAssertEqual(page.items.map(\.request.title), ["Pick the storage layer", "Assume UTC timestamps", "Which name?"])
        XCTAssertEqual(page.items.map(\.request.kind), ["decision", "assumption", "question"])
        XCTAssertEqual(page.items.map(\.request.blocking), [true, false, false])
        XCTAssertEqual(page.items[0].chairSessionId, "3f9c2a10d4e84b7f")
        XCTAssertNil(page.nextCursor)
        XCTAssertEqual(replay.recorded.count, 2)
        XCTAssertTrue(replay.recorded.allSatisfy { $0.contains("--all-projects") && $0.last == "--json" })
    }

    func testHostWideListNamesEachRequestsProject() async throws {
        let replay = Replay([(["inbox"], "inbox-host", 0)])
        let page = try await InboxClient(cli: replay.client).list()
        XCTAssertEqual(page.items.map(\.projectId), ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"])
        XCTAssertEqual(page.items.map(\.projectName), ["provenant", "provenant"])
        XCTAssertEqual(replay.recorded.count, 1)
    }

    func testFirstPageAsksForPeersAndLaterPagesDoNot() async throws {
        let replay = Replay([(["inbox", "--all-projects", "--state", "open", "--limit", "50", "--cursor", "CUR1"], "inbox-page2", 0),
                             (["inbox"], "inbox-page1", 0)])
        _ = try await InboxClient(cli: replay.client).list()
        XCTAssertEqual(replay.recorded[0], ["inbox", "--all-projects", "--state", "open", "--limit", "50", "--hosts", "all", "--json"])
        XCTAssertEqual(replay.recorded[1], ["inbox", "--all-projects", "--state", "open", "--limit", "50", "--cursor", "CUR1", "--json"])
        XCTAssertFalse(replay.recorded[1].contains("--hosts"))
    }

    func testPeerListingGetsALongerBudgetThanThePeerDeadline() async throws {
        let seen = LockedBox<[(hosts: Bool, limit: Double)]>([])
        let client = CLIClient(limit: 30) { arguments, limit in
            seen.update { $0.append((arguments.contains("--hosts"), limit)) }
            let file = arguments.contains("--cursor") ? "inbox-page2" : "inbox-page1"
            return CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false,
                              out: try Replay.fixture(file), err: "")
        }
        let page = try await InboxClient(cli: client).list()
        XCTAssertEqual(page.items.count, 3)
        let calls = seen.value
        XCTAssertEqual(calls.map(\.hosts), [true, false])
        // The default limits.peer_seconds is 30; the call that fans out to peers must outlast it.
        XCTAssertGreaterThan(calls[0].limit, 30)
        XCTAssertEqual(calls[1].limit, 30)
        let other = CLIClient(limit: 30) { _, limit in
            XCTAssertEqual(limit, 30)
            return CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false, out: try Replay.fixture("answer-conflict"), err: "")
        }
        _ = try? await other.call(["answer"], command: "answer", as: InboxReceipt.self)
    }

    func testPeerRowsCarryTheirHostLabelProjectAndCoverage() async throws {
        let replay = Replay([(["inbox"], "inbox-peers-complete", 0)])
        let page = try await InboxClient(cli: replay.client).list()
        XCTAssertEqual(page.items.map(\.id.first), ["1", "5", "6"])
        let local = page.items[0], peer = page.items[1]
        XCTAssertEqual(local.ownerHost, "cccccccccccccccccccccccccccccccc")
        XCTAssertNil(local.peerHost)
        XCTAssertNil(local.peerRoute)
        XCTAssertEqual(peer.ownerHost, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
        XCTAssertEqual(peer.peerHost, peer.ownerHost)
        XCTAssertEqual(peer.peerLabel, "studio")
        XCTAssertEqual(peer.peerRoute, PeerRoute(host: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", projectKey: "peer/key"))
        XCTAssertEqual(peer.projectName, "alpha")
        XCTAssertEqual(page.peers?.map(\.state), ["complete", "unsupported"])
        XCTAssertEqual(page.peers?.first?.included, 2)
        XCTAssertEqual(page.peers?.first?.kind, .complete)
    }

    func testCoverageStaysAStringAndUnknownPeerStatesDecode() async throws {
        let listed = try CLIClient.decode(
            CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false,
                       out: try Replay.fixture("inbox-peers-future"), err: ""),
            command: "inbox", as: InboxClient.Listed.self)
        XCTAssertTrue(listed.coverage.hasPrefix("local_only"))
        let peer = try XCTUnwrap(listed.peers?.first)
        XCTAssertEqual(peer.state, "degraded")
        XCTAssertEqual(peer.kind, .other("degraded"))
        XCTAssertFalse(peer.contributedNoPage)
        let gaps = try JSONDecoder.snake.decode(PeerCoverage.self, from: Data(
            #"{"host":"h","label":"l","state":"partial","included":0,"invalid_rows":1,"duplicate_rows":2,"truncated":true,"reason":"invalid_page"}"#.utf8))
        XCTAssertEqual([gaps.invalidRows, gaps.duplicateRows], [1, 2])
        XCTAssertTrue(gaps.truncated && gaps.contributedNoPage)
        let noReason = PeerCoverage(host: "h", label: "l", state: "partial", included: 3, truncated: true)
        XCTAssertFalse(noReason.contributedNoPage)
        XCTAssertTrue(PeerCoverage(host: "h", label: "l", state: "unreachable", reason: "timeout").contributedNoPage)
        // An unknown state with no page needs a reason before it counts as a refused page.
        XCTAssertFalse(PeerCoverage(host: "h", label: "l", state: "degraded").contributedNoPage)
        XCTAssertTrue(PeerCoverage(host: "h", label: "l", state: "degraded", reason: "busy").contributedNoPage)
        XCTAssertFalse(PeerCoverage(host: "h", label: "l", state: "degraded", included: 2, reason: "busy").contributedNoPage)
    }

    func testPeerAnswerAndViewNameTheOwnerHostAndProjectKey() async throws {
        let route = PeerRoute(host: "eeee", projectKey: "peer/key")
        let replay = Replay([(["answer"], "answer-saved", 0), (["inbox", "--all-projects", "9"], "inbox-item-answered", 0)])
        let client = InboxClient(cli: replay.client)
        _ = try await client.answer("9", action: .choose, option: "sqlite", expectRevision: 3, route: route)
        XCTAssertEqual(replay.recorded[0], ["answer", "--all-projects", "9", "--host", "eeee", "--project-key", "peer/key",
                                            "--action", "choose", "--expect-revision", "3", "--channel", "console", "--option", "sqlite", "--json"])
        _ = try await client.item("9", route: route)
        XCTAssertEqual(replay.recorded[1], ["inbox", "--all-projects", "9", "--host", "eeee", "--project-key", "peer/key", "--json"])
        replay.set([(["answer"], "answer-conflict", 1), (["inbox", "--all-projects", "9"], "inbox-item-stale", 0)])
        let outcome = try await client.answer("9", action: .confirm, expectRevision: 3, route: route)
        guard case .stale = outcome else { return XCTFail("expected stale") }
        XCTAssertEqual(replay.recorded.last, ["inbox", "--all-projects", "9", "--host", "eeee", "--project-key", "peer/key", "--json"])
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
        XCTAssertEqual(call, ["answer", "--all-projects", "1", "--action", "choose", "--expect-revision", "3", "--channel", "console", "--option", "sqlite", "--json"])
    }

    func testStaleRevisionReadsTheCurrentStateAndWritesNothing() async throws {
        let replay = Replay([(["answer"], "answer-conflict", 1), (["inbox", "--all-projects", "1"], "inbox-item-stale", 0)])
        let outcome = try await InboxClient(cli: replay.client).answer("1", action: .confirm, expectRevision: 3)
        guard case .stale(let current) = outcome else { return XCTFail("expected a stale outcome") }
        XCTAssertTrue(replay.recorded.allSatisfy { $0.contains("--all-projects") })
        XCTAssertEqual(current.request.revision, 5)
        XCTAssertEqual(current.request.state, "superseded")
        XCTAssertEqual(replay.recorded.filter { $0.first == "answer" }.count, 1)
    }

    func testItemCarriesAnswerAndAcknowledgement() async throws {
        let replay = Replay([(["inbox", "--all-projects", "1"], "inbox-item-answered", 0)])
        let item = try await InboxClient(cli: replay.client).item("1")
        XCTAssertEqual(item.answer?.channel, "console")
        XCTAssertEqual(item.delivery?.state, "acknowledged")
        XCTAssertEqual(item.delivery?.acknowledgedAt, "2026-10-30T01:06:09Z")
    }

    func testErrorEnvelopeBecomesTypedError() async throws {
        let replay = Replay([(["answer"], "answer-conflict", 1)])
        do {
            _ = try await replay.client.call(["answer"], command: "answer", as: InboxReceipt.self)
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
            do { _ = try await client.call(["inbox"], command: "inbox", as: InboxReceipt.self); XCTFail("expected an error") } catch let error as ClientError {
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

final class ExecTimeoutTests: XCTestCase {
    func testOverrunIsTerminatedAndFlagged() throws {
        let started = Date()
        let run = try CLI.exec(URL(fileURLWithPath: "/bin/sleep"), ["30"], path: nil, limit: 0.3, grace: 0.3)
        XCTAssertTrue(run.timedOut)
        XCTAssertTrue(run.signalled)
        XCTAssertLessThan(Date().timeIntervalSince(started), 10)
    }

    func testDescendantsIgnoringTermAreKilledAfterTheGrace() throws {
        // Publish readiness after the shell installs its trap and forks sleep.
        let marker = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: marker) }
        let out = Pipe(), err = Pipe()
        let command = "trap '' TERM; sleep 30 & echo $! > \"$1\"; wait"
        let pid = try CLI.spawn(URL(fileURLWithPath: "/bin/sh"), ["-c", command, "descendant-test", marker.path],
                                path: nil, qos: QOS_CLASS_UTILITY, out: out, err: err)
        let leader = CLI.Leader(pid: pid)
        var childPID: pid_t?
        let ready = waitUntil(timeout: 5) {
            guard let child = try? String(contentsOf: marker, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines) else { return false }
            childPID = pid_t(child)
            return childPID != nil
        }
        guard ready, let childPID else {
            leader.signalGroup(SIGKILL)
            if leader.waitForExit(seconds: 3) { _ = leader.reap() } else { leader.abandon() }
            out.fileHandleForReading.closeFile()
            err.fileHandleForReading.closeFile()
            return XCTFail("shell did not publish its descendant PID")
        }

        let leaderStillRunning = CLI.terminate(leader, grace: 0.3)
        if leaderStillRunning { leader.abandon() } else { _ = leader.reap() }
        out.fileHandleForReading.closeFile()
        err.fileHandleForReading.closeFile()
        XCTAssertFalse(leaderStillRunning)
        XCTAssertTrue(waitUntil(timeout: 5, condition: { !processExists(childPID) }),
                      "SIGKILL should end the descendant after the grace period")
    }

    private func waitUntil(timeout: TimeInterval, condition: () -> Bool) -> Bool {
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        while !condition() && ProcessInfo.processInfo.systemUptime < deadline {
            Thread.sleep(forTimeInterval: 0.01)
        }
        return condition()
    }

    private func processExists(_ pid: pid_t) -> Bool {
        if kill(pid, 0) == 0 { return true }
        return errno != ESRCH
    }

    func testCleanExitReportsStatusAndOutput() throws {
        let run = try CLI.exec(URL(fileURLWithPath: "/bin/sh"), ["-c", "echo hi; echo oops >&2; exit 3"], path: nil, limit: 5)
        XCTAssertEqual(run.status, 3)
        XCTAssertFalse(run.timedOut)
        XCTAssertFalse(run.signalled)
        XCTAssertEqual(String(decoding: run.out, as: UTF8.self), "hi\n")
        XCTAssertEqual(run.err, "oops\n")
    }
}

final class LockedBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Value
    init(_ value: Value) { stored = value }
    var value: Value { lock.withLock { stored } }
    func update(_ change: (inout Value) -> Void) { lock.withLock { change(&stored) } }
}
