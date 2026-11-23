import Foundation
import XCTest
@testable import QuotaMenuBar

final class SingleInstanceTests: XCTestCase {
    private func location() -> (directory: URL, marker: URL) {
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "provenant-single-instance-\(UUID().uuidString)", directoryHint: .isDirectory)
        return (directory, directory.appending(path: "instance", directoryHint: .notDirectory))
    }

    func testMarkerReadsTheOwnerAndRejectsUnusableFiles() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        try Data("4242\nnounce".utf8).write(to: marker)
        XCTAssertEqual(InstanceMarker(url: marker), InstanceMarker(pid: 4242, nonce: "nounce"))

        for unusable in ["not-a-pid\nnounce", "4242\n", "4242"] {
            try Data(unusable.utf8).write(to: marker)
            XCTAssertNil(InstanceMarker(url: marker), unusable)
        }
        XCTAssertNil(InstanceMarker(url: directory.appending(path: "missing", directoryHint: .notDirectory)))
    }

    func testTakeOverTerminatesALiveOlderOwnerAndPublishesTheNewOne() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        try InstanceMarker(pid: 111, nonce: "old").write(to: marker)
        var terminated: [pid_t] = []
        var livenessChecks = 0
        let won = SingleInstance.takeOver(
            url: marker,
            pid: 222,
            terminate: { terminated.append($0) },
            isAlive: { pid in
                livenessChecks += 1
                return pid == 111 && livenessChecks == 1
            },
            isMenuBar: { $0 == 111 },
            delay: { _ in }
        )
        XCTAssertTrue(won)
        XCTAssertEqual(terminated, [111])
        let owner = try XCTUnwrap(InstanceMarker(url: marker))
        XCTAssertEqual(owner.pid, 222)
        XCTAssertFalse(owner.nonce.isEmpty)
        XCTAssertNotEqual(owner.nonce, "old")
    }

    func testTakeOverIgnoresItselfAndStaleMarkers() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        var terminated: [pid_t] = []

        try InstanceMarker(pid: 222, nonce: "self").write(to: marker)
        let selfWon = SingleInstance.takeOver(
            url: marker, pid: 222,
            terminate: { terminated.append($0) },
            isAlive: { _ in true },
            isMenuBar: { _ in true },
            delay: { _ in }
        )
        XCTAssertTrue(selfWon)
        XCTAssertTrue(terminated.isEmpty)

        try InstanceMarker(pid: 111, nonce: "dead").write(to: marker)
        let deadWon = SingleInstance.takeOver(
            url: marker, pid: 222,
            terminate: { terminated.append($0) },
            isAlive: { _ in false },
            isMenuBar: { _ in true },
            delay: { _ in }
        )
        XCTAssertTrue(deadWon)
        XCTAssertTrue(terminated.isEmpty)
        XCTAssertEqual(InstanceMarker(url: marker)?.pid, 222)

        try Data("broken".utf8).write(to: marker)
        let brokenWon = SingleInstance.takeOver(
            url: marker, pid: 222,
            terminate: { terminated.append($0) },
            isAlive: { _ in true },
            isMenuBar: { _ in true },
            delay: { _ in }
        )
        XCTAssertTrue(brokenWon)
        XCTAssertTrue(terminated.isEmpty)
        XCTAssertEqual(InstanceMarker(url: marker)?.pid, 222)
    }

    func testTakeOverDoesNotSignalAPidReusedByAnotherExecutable() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        try InstanceMarker(pid: 111, nonce: "old").write(to: marker)
        var terminated: [pid_t] = []
        let won = SingleInstance.takeOver(
            url: marker,
            pid: 222,
            nonce: "new",
            terminate: { terminated.append($0) },
            isAlive: { _ in true },
            isMenuBar: { _ in false }, // PID 111 now belongs to an unrelated executable
            delay: { _ in }
        )
        XCTAssertTrue(terminated.isEmpty, "an unverified PID must never be signalled")
        XCTAssertTrue(won, "an unverified owner is replaced, not waited on")
        XCTAssertEqual(InstanceMarker(url: marker), InstanceMarker(pid: 222, nonce: "new"))
    }

    func testTakeOverReplacesAMatchingOwner() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        try InstanceMarker(pid: 111, nonce: "old").write(to: marker)
        var terminated: [pid_t] = []
        var livenessChecks = 0
        let won = SingleInstance.takeOver(
            url: marker,
            pid: 222,
            nonce: "new",
            terminate: { terminated.append($0) },
            isAlive: { pid in
                livenessChecks += 1
                return pid == 111 && livenessChecks == 1 // live until signalled
            },
            isMenuBar: { $0 == 111 }, // PID 111 still runs this app's executable
            delay: { _ in }
        )
        XCTAssertEqual(terminated, [111])
        XCTAssertTrue(won)
        XCTAssertEqual(InstanceMarker(url: marker), InstanceMarker(pid: 222, nonce: "new"))
    }

    func testConcurrentStartRaceLeavesOneOwner() throws {
        let (directory, marker) = location()
        defer { try? FileManager.default.removeItem(at: directory) }
        // A competing launch publishes its marker while this process waits to
        // confirm ownership; only the surviving marker may keep running.
        let other = InstanceMarker(pid: 333, nonce: "other")
        var competitorWrote = false
        let won = SingleInstance.takeOver(
            url: marker,
            pid: 222,
            nonce: "mine",
            terminate: { _ in XCTFail("no verified owner should be terminated") },
            isAlive: { _ in false },
            isMenuBar: { _ in false },
            delay: { interval in
                guard interval > 0, !competitorWrote else { return }
                competitorWrote = true
                try? other.write(to: marker)
            }
        )
        XCTAssertFalse(won, "the launch whose marker was overwritten must yield")
        XCTAssertEqual(InstanceMarker(url: marker), other, "the competing marker is the sole owner")
    }
}
