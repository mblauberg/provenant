import XCTest
@testable import QuotaMenuBar

final class ConsoleAppTests: XCTestCase {
    private let outer = "/Applications/Provenant Console.app"

    func testFindsTheConsoleAppAroundTheNestedAgent() {
        let agent = URL(fileURLWithPath: "\(outer)/Contents/Library/LoginItems/Provenant Menu.app")
        let found = ConsoleApp.enclosingApp(of: agent) { $0.path == self.outer ? "dev.provenant.console" : "dev.provenant.menu" }
        XCTAssertEqual(found?.path, outer)
    }

    func testIgnoresTheAgentItselfAndOtherAppsAbove() {
        let agent = URL(fileURLWithPath: "/Users/me/Apps/Other.app/Contents/Library/LoginItems/Menu.app")
        XCTAssertNil(ConsoleApp.enclosingApp(of: agent) { _ in "dev.provenant.menu" })
        // The agent's own identifier never matches, even if it were the console's.
        let alone = URL(fileURLWithPath: "/tmp/build/quota-menubar")
        XCTAssertNil(ConsoleApp.enclosingApp(of: alone) { _ in nil })
    }
}
