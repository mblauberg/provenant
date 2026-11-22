import XCTest
@testable import ProvenantKit

/// A lane is named for its graph: the node is the default `task` on a single-node graph, and the attempt only
/// distinguishes retries. Every surface (overview, runs, tree, detail, menu) uses this one rule.
final class LaneTitleTests: XCTestCase {
    func testDefaultNodeAndFirstAttemptAreLeftOut() {
        XCTAssertEqual(LaneTitle.title(graph: "lane-default", node: "task", attempt: 1), "lane-default")
        XCTAssertEqual(LaneTitle.title(graph: "lane-default", node: nil, attempt: nil), "lane-default")
    }

    func testNamedNodeAndRetryAreShown() {
        XCTAssertEqual(LaneTitle.title(graph: "lane-retry", node: "task--writer", attempt: 1),
                       "lane-retry · task--writer")
        XCTAssertEqual(LaneTitle.title(graph: "lane-retry", node: "task", attempt: 3),
                       "lane-retry · attempt 3")
        XCTAssertEqual(LaneTitle.title(graph: "lane-retry", node: "task--writer", attempt: 2),
                       "lane-retry · task--writer · attempt 2")
    }

    func testAnUnnamedGraphFallsBackToItsNodeAndThenNothing() {
        XCTAssertEqual(LaneTitle.title(graph: nil, node: "task--writer", attempt: 2), "task--writer · attempt 2")
        XCTAssertNil(LaneTitle.title(graph: nil, node: "task", attempt: 1))
        XCTAssertNil(LaneTitle.title(nil))
    }
}
