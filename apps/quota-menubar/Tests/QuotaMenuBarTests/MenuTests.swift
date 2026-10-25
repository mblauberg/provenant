import XCTest
@testable import QuotaMenuBar

final class MenuTests: XCTestCase {
    private func fixture() throws -> QuotaReport {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appending(path: "Fixtures/current.json")
        return try CLI.decode(Data(contentsOf: url)).get()
    }

    private func entries(_ report: QuotaReport, refreshing: Bool = false) -> [MenuEntry] {
        QuotaMenu.entries(report: report, error: nil, updatedAt: report.generatedAt, refreshing: refreshing, lanes: [],
                          stateRoot: URL(fileURLWithPath: "/tmp/provenant"), now: report.generatedAt)
    }

    private func items(_ entries: [MenuEntry]) -> [MenuItemSpec] {
        entries.compactMap { if case .item(let spec) = $0 { spec } else { nil } }
    }

    private func row(_ id: String) throws -> MenuItemSpec {
        try XCTUnwrap(items(entries(try fixture())).first { $0.id == id })
    }

    func testTitleRowCarriesFreshness() throws {
        let report = try fixture()
        XCTAssertEqual(entries(report).first, .title("Quota", "Updated just now"))
        XCTAssertEqual(entries(report, refreshing: true).first, .title("Quota", "Refreshing…"))
    }

    func testAccountsStayTogetherByProviderInStatusOrder() throws {
        let rows = items(entries(try fixture())).filter { $0.id != nil }
        XCTAssertEqual(rows.map(\.title), ["Claude", "Codex", "OpenCode", "Antigravity", "Cursor"])
        // No per-provider section headers: the provider names each row.
        XCTAssertFalse(entries(try fixture()).contains { if case .header = $0 { true } else { false } })
    }

    func testAccountRowShowsBindingUseAndResetWithASubmenu() throws {
        let row = try row("claude/anese")
        XCTAssertEqual(row.title, "Claude")
        XCTAssertEqual(row.detail, "Anese")
        XCTAssertEqual(row.value, "84%")
        XCTAssertEqual(row.subtitle, "Claude-5h · resets in 3h")
        XCTAssertEqual(row.icon, .ring(0.84, .green))
        let windows = try XCTUnwrap(row.submenu).compactMap { if case .window(let window) = $0 { window } else { nil } }
        XCTAssertEqual(windows.map(\.value), ["84%", "52%"])
    }

    func testLabelsThatRepeatTheProviderAreDropped() throws {
        XCTAssertNil(try row("codex/default").detail)
        XCTAssertEqual(try row("cursor/cursor-test-01").detail, "Profile")
    }

    func testSpentAllowanceShowsWhenItIsBackWithoutAFigure() throws {
        let exhausted = try row("agy/google-test-01")
        XCTAssertNil(exhausted.value)
        XCTAssertEqual(exhausted.icon, .ring(1, .red))
        XCTAssertTrue(exhausted.subtitle?.hasPrefix("Back ") == true)
        let credits = try row("codex/default")
        XCTAssertNil(credits.value)
        XCTAssertTrue(credits.subtitle?.hasPrefix("On credits") == true)
    }

    func testStaleRowsDimTheirFigureAndShowTheirAge() throws {
        let stale = try row("opencode/local-test-01")
        XCTAssertTrue(stale.dimValue)
        XCTAssertTrue(stale.subtitle?.hasSuffix(" · last checked 1d ago") == true)
        // A reading three minutes old is fresh, so its row carries no age.
        XCTAssertFalse(try row("claude/anese").subtitle?.contains("ago") ?? true)
    }

    func testRowsWithASubmenuHaveNoTooltip() throws {
        let rows = items(entries(try fixture())).filter { $0.submenu != nil }
        XCTAssertFalse(rows.isEmpty)
        XCTAssertTrue(rows.allSatisfy { $0.help == nil })
    }

    func testSubmenuDoesNotRepeatTheRowsState() throws {
        let details = try XCTUnwrap(try row("opencode/local-test-01").submenu)
        let facts = details.compactMap { if case .fact(let key, _) = $0 { key } else { nil } }
        XCTAssertFalse(facts.contains("Status"))
        XCTAssertEqual(facts, ["Checked"])
    }

    func testFooterHasRefreshReportFolderAndQuitWithHiddenShortcuts() throws {
        let footer = items(entries(try fixture())).suffix(5)
        let visible = footer.filter { !$0.hidden }, hidden = footer.filter(\.hidden)
        XCTAssertEqual(visible.map(\.title), ["Refresh", "Open Report Folder", "Quit Quota"])
        XCTAssertEqual(visible.map(\.shortcut), [nil, nil, nil])
        XCTAssertEqual(hidden.map(\.shortcut), ["r", "q"])
        XCTAssertEqual(hidden.map(\.action), [.refresh, .quit])
        XCTAssertEqual(visible[1].action, .open(URL(fileURLWithPath: "/tmp/provenant/quota/report.json").deletingLastPathComponent()))
    }
}
