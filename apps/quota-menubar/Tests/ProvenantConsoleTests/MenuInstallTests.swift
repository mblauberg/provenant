import Foundation
import XCTest
@testable import ProvenantConsole

final class MenuInstallTests: XCTestCase {
    func testAtomicSwapAndRollbackKeepBothBundlesAndFailedSwapPreservesInstalledApp() throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let installed = directory.appending(path: "installed.app")
        let staged = directory.appending(path: "staged.app")
        for (path, value) in [(installed, "old"), (staged, "new")] {
            try FileManager.default.createDirectory(at: path, withIntermediateDirectories: true)
            try Data(value.utf8).write(to: path.appending(path: "build"))
        }
        XCTAssertEqual(Main.swapPaths(staged.path, installed.path), 0)
        XCTAssertEqual(try String(contentsOf: installed.appending(path: "build"), encoding: .utf8), "new")
        XCTAssertEqual(try String(contentsOf: staged.appending(path: "build"), encoding: .utf8), "old")
        XCTAssertEqual(Main.swapPaths(staged.path, installed.path), 0)
        XCTAssertNotEqual(Main.swapPaths(directory.appending(path: "missing").path, installed.path), 0)
        XCTAssertEqual(try String(contentsOf: installed.appending(path: "build"), encoding: .utf8), "old")
    }
}
