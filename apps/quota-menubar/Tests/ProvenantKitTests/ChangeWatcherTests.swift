import Foundation
import XCTest
import ProvenantTestSupport
@testable import ProvenantKit

@MainActor
final class ChangeWatcherTests: XCTestCase {
    /// Hands out the recorded changes one after another and records the baseline each read was given.
    private final class Feed: @unchecked Sendable {
        private let lock = NSLock()
        private var since: [Generations?] = []
        var seen: [Generations?] { lock.withLock { since } }
        func read(_ baseline: Generations?) async throws -> Changes {
            lock.withLock { since.append(baseline) }
            let name = lock.withLock { since.count } == 1 ? "console-changes" : "console-changes-after-run-write"
            return try JSONDecoder.snake.decode(Envelope<Changes>.self, from: try Replay.fixture(name)).data!
        }
    }

    func testFirstAnswerIsABaselineAndLaterOnesCarryTheLastGenerations() async throws {
        let feed = Feed()
        var answers: [(ChangedScopes, Bool)] = []
        let second = expectation(description: "second answer")
        let watcher = ChangeWatcher(stateDirectory: URL(fileURLWithPath: "/nonexistent/state"), read: feed.read, opener: { _ in -1 }) { changed, baseline in
            answers.append((changed, baseline))
            if answers.count == 2 { second.fulfill() }
        }
        watcher.start()
        await Task.yield()
        // Wait for the first read to finish before signalling again, as an owner write would.
        while answers.isEmpty { await Task.yield() }
        watcher.eventOccurred()
        await fulfillment(of: [second], timeout: 5)
        watcher.stop()
        XCTAssertTrue(answers[0].1)
        XCTAssertNil(answers[0].0.runs)
        XCTAssertFalse(answers[1].1)
        XCTAssertEqual(answers[1].0.runs, true)
        XCTAssertNil(feed.seen[0])
        XCTAssertEqual(feed.seen[1]?.board, 14)
    }

    func testEventsDuringARead_FoldIntoOneMoreRead() async throws {
        let gate = DispatchSemaphore(value: 0)
        let count = Counter()
        let done = expectation(description: "reads settle")
        let watcher = ChangeWatcher(stateDirectory: URL(fileURLWithPath: "/nonexistent/state"), read: { _ in
            let n = count.next()
            if n == 1 { gate.wait() }
            return try JSONDecoder.snake.decode(Envelope<Changes>.self, from: try Replay.fixture("console-changes")).data!
        }, opener: { _ in -1 }) { _, _ in if count.value == 2 { done.fulfill() } }
        watcher.start()
        while count.value < 1 { await Task.yield() }
        for _ in 0..<5 { watcher.eventOccurred() }   // five events while the first read is out
        gate.signal()
        await fulfillment(of: [done], timeout: 5)
        watcher.stop()
        XCTAssertEqual(count.value, 2)
    }

    func testAWriteToTheSignalFileWakesTheWatcher() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "watch-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let signal = StateDirectory.signalFile(in: dir)
        try Data("{}".utf8).write(to: signal)
        let count = Counter()
        let woke = expectation(description: "woke after the write")
        woke.assertForOverFulfill = false
        let watcher = ChangeWatcher(stateDirectory: dir, read: { _ in
            count.next()
            return try JSONDecoder.snake.decode(Envelope<Changes>.self, from: try Replay.fixture("console-changes")).data!
        }) { _, baseline in if !baseline { woke.fulfill() } }
        watcher.start()
        while count.value < 1 { await Task.yield() }
        try Data("{\"board\":2}".utf8).write(to: signal, options: .atomic)   // a replace, as well as a write, is heard
        await fulfillment(of: [woke], timeout: 5)
        watcher.stop()
    }

    func testStateDirectoryFollowsTheEnvironment() {
        XCTAssertEqual(StateDirectory.resolve(environment: ["PROVENANT_STATE_DIR": "/tmp/state"]).path, "/tmp/state")
        XCTAssertEqual(StateDirectory.resolve(environment: ["PROVENANT_STATE_DIR": "relative"], home: URL(fileURLWithPath: "/home/me")).path,
                       "/home/me/.local/share/provenant/v2")
        XCTAssertEqual(StateDirectory.signalFile(in: URL(fileURLWithPath: "/s")).path, "/s/changes.json")
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    var value: Int { lock.withLock { n } }
    @discardableResult func next() -> Int { lock.withLock { n += 1; return n } }
}

extension JSONDecoder {
    static var snake: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }
}
