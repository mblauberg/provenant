import XCTest
@testable import QuotaMenuBar

final class StateRootTests: XCTestCase {
    func testEnvironmentStateRoot() {
        XCTAssertEqual(LaneBoard.stateRoot(report: nil, environment: ["PROVENANT_STATE_DIR": "/tmp/state"]).path, "/tmp/state")
    }
}
