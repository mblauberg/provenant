import XCTest
@testable import QuotaMenuBar

final class MenuTests: XCTestCase {
    private func fixture() throws -> QuotaReport {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appending(path: "Fixtures/current.json")
        return try CLI.decode(Data(contentsOf: url)).get()
    }

    private func entries(_ report: QuotaReport) -> [MenuEntry] {
        QuotaMenu.entries(report: report, error: nil, updatedAt: report.generatedAt, refreshing: false, lanes: [],
                          stateRoot: URL(fileURLWithPath: "/tmp/provenant"), now: report.generatedAt)
    }

    private func items(_ entries: [MenuEntry]) -> [MenuItemSpec] {
        entries.compactMap { if case .item(let spec) = $0 { spec } else { nil } }
    }

    func testOneSectionPerAdapterInStatusOrder() throws {
        let headers = entries(try fixture()).compactMap { if case .header(let title) = $0 { title } else { nil } }
        XCTAssertEqual(headers, ["Claude", "Codex", "OpenCode", "Antigravity", "Cursor"])
    }

    func testAccountRowShowsBindingUseAndResetWithASubmenu() throws {
        let row = try XCTUnwrap(items(entries(try fixture())).first { $0.id == "claude/anese" })
        XCTAssertEqual(row.title, "Anese")
        XCTAssertEqual(row.badge, "84%")
        XCTAssertEqual(row.subtitle, "Claude-5h · resets in 3h")
        XCTAssertEqual(items(try XCTUnwrap(row.submenu)).map(\.badge).prefix(2), ["84%", "52%"])
    }

    func testStaleRowsShowTheirAge() throws {
        let rows = items(entries(try fixture()))
        let stale = try XCTUnwrap(rows.first { $0.id == "opencode/local-test-01" })
        XCTAssertTrue(stale.subtitle?.hasPrefix("Stale · ") == true)
        XCTAssertTrue(stale.subtitle?.hasSuffix(" · 1d ago") == true)
        // A reading three minutes old is fresh, so its row carries no age.
        XCTAssertFalse(rows.first { $0.id == "claude/anese" }?.subtitle?.contains("ago") ?? true)
    }

    func testFooterHasRefreshReportFolderAndQuit() throws {
        let footer = items(entries(try fixture())).suffix(4)
        XCTAssertEqual(footer.map(\.title).suffix(3), ["Refresh", "Open Report Folder", "Quit Quota"])
        XCTAssertEqual(footer.map(\.shortcut).suffix(3), ["r", nil, "q"])
        XCTAssertEqual(footer.dropFirst(2).first?.action, .open(URL(fileURLWithPath: "/tmp/provenant/quota/report.json").deletingLastPathComponent()))
    }
}
