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
        SingleInstance.takeOver(
            url: marker,
            pid: 222,
            terminate: { terminated.append($0) },
            isAlive: { pid in
                livenessChecks += 1
                return pid == 111 && livenessChecks == 1
            }
        )
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
        SingleInstance.takeOver(url: marker, pid: 222, terminate: { terminated.append($0) }, isAlive: { _ in true })
        XCTAssertTrue(terminated.isEmpty)

        try InstanceMarker(pid: 111, nonce: "dead").write(to: marker)
        SingleInstance.takeOver(url: marker, pid: 222, terminate: { terminated.append($0) }, isAlive: { _ in false })
        XCTAssertTrue(terminated.isEmpty)
        XCTAssertEqual(InstanceMarker(url: marker)?.pid, 222)

        try Data("broken".utf8).write(to: marker)
        SingleInstance.takeOver(url: marker, pid: 222, terminate: { terminated.append($0) }, isAlive: { _ in true })
        XCTAssertTrue(terminated.isEmpty)
        XCTAssertEqual(InstanceMarker(url: marker)?.pid, 222)
    }
}
