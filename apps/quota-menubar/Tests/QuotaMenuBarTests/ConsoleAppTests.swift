import XCTest
@testable import QuotaMenuBar

final class ConsoleAppTests: XCTestCase {
    private let outer = "/Applications/Provenant Console.app"

    func testFindsTheConsoleAppAroundTheMenuHelper() {
        let helper = URL(fileURLWithPath: "\(outer)/Contents/MacOS/quota-menubar")
        let found = ConsoleApp.enclosingApp(of: helper) { $0.path == self.outer ? "dev.provenant.console" : nil }
        XCTAssertEqual(found?.path, outer)
    }

    func testIgnoresOtherAppsAboveAnUnrelatedHelper() {
        let helper = URL(fileURLWithPath: "/Users/me/Apps/Other.app/Contents/MacOS/quota-menubar")
        XCTAssertNil(ConsoleApp.enclosingApp(of: helper) { _ in nil })
        let alone = URL(fileURLWithPath: "/tmp/build/quota-menubar")
        XCTAssertNil(ConsoleApp.enclosingApp(of: alone) { _ in nil })
    }
}
