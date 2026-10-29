import Foundation
import XCTest

/// The console and the shared client read the CLI's JSON and nothing else.
final class SourceBoundaryTests: XCTestCase {
    private let sources = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appending(path: "../../Sources").standardized

    private func swiftFiles(in target: String) throws -> [(name: String, text: String)] {
        let root = sources.appending(path: target)
        let files = try XCTUnwrap(FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil))
        return try files.compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
            .map { ($0.lastPathComponent, try String(contentsOf: $0, encoding: .utf8)) }
    }

    func testSharedAndConsoleSourcesTouchNoStoreCredentialOrSSH() throws {
        let forbidden = ["sqlite", "import Security", "SecItem", "Keychain", "ssh", "import GRDB", ".db\"", "credentials", ".claude", ".codex"]
        for target in ["ProvenantKit", "ProvenantConsole"] {
            let files = try swiftFiles(in: target)
            XCTAssertFalse(files.isEmpty)
            for file in files {
                for word in forbidden {
                    XCTAssertFalse(file.text.lowercased().contains(word.lowercased()), "\(target)/\(file.name) mentions \(word)")
                }
            }
        }
    }

    func testConsoleRunsNoTimers() throws {
        for file in try swiftFiles(in: "ProvenantConsole") {
            for word in ["Timer", "asyncAfter", "Task.sleep", "DispatchSource"] {
                XCTAssertFalse(file.text.contains(word), "\(file.name) uses \(word)")
            }
        }
    }

    func testMenuTargetHoldsNoConsoleCode() throws {
        for file in try swiftFiles(in: "QuotaMenuBar") {
            for word in ["import ProvenantConsole", "import Charts", "InboxClient", "InboxModel"] {
                XCTAssertFalse(file.text.contains(word), "\(file.name) mentions \(word)")
            }
        }
    }
}
