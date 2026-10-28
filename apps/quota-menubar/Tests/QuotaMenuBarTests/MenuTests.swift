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

    private func allItems(_ entries: [MenuEntry]) -> [MenuItemSpec] {
        let rows = items(entries)
        return rows + rows.flatMap { allItems($0.submenu ?? []) }
    }

    private func row(_ id: String) throws -> MenuItemSpec {
        try XCTUnwrap(allItems(entries(try fixture())).first { $0.id == id })
    }

    func testTitleRowCarriesFreshness() throws {
        let report = try fixture()
        XCTAssertEqual(entries(report).first, .title("Quota", "Updated just now"))
        XCTAssertEqual(entries(report, refreshing: true).first, .title("Quota", "Refreshing…"))
    }

    func testAccountsStayTogetherByProviderInStatusOrder() throws {
        let rows = items(entries(try fixture())).filter { $0.id != nil }
        XCTAssertEqual(rows.map(\.title), ["Claude", "Codex", "OpenCode", "Cursor"])
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

    func testAuthCheckFailureAndStaleAreDistinct() throws {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appending(path: "Fixtures/current.json")
        var json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        var accounts = try XCTUnwrap(json["accounts"] as? [[String: Any]])
        accounts[0]["status"] = "unknown"
        accounts[0]["error"] = "probe failed"
        json["accounts"] = accounts
        let report = try CLI.decode(JSONSerialization.data(withJSONObject: json)).get()
        let rows = allItems(entries(report))
        XCTAssertEqual(rows.first { $0.id == "claude/anese" }?.subtitle, "Couldn’t check")
        XCTAssertEqual(rows.first { $0.id == "cursor/cursor-test-01" }?.subtitle, "Sign-in required")
        XCTAssertTrue(rows.first { $0.id == "opencode/local-test-01" }?.subtitle?.contains("last checked") == true)
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

    func testFooterHasConsoleHintAndSettings() throws {
        let rows = items(entries(try fixture()))
        let console = try XCTUnwrap(rows.first { $0.title == "Open Console" })
        XCTAssertFalse(console.enabled)
        XCTAssertTrue(console.subtitle?.contains("provenant inbox") == true)
        let settings = try XCTUnwrap(rows.first { $0.title == "Settings" }?.submenu)
        XCTAssertTrue(items(settings).contains { $0.title == "Open Report Folder" })
        XCTAssertTrue(items(settings).contains { $0.action == .quit })
        XCTAssertEqual(rows.filter(\.hidden).map(\.shortcut), ["r", "q"])
    }

    func testOverviewLeadsWithThreeRequestsAndUsefulLabels() throws {
        let json = """
        {"schema_version":1,"command":"console overview","status":"ok","data":{
          "needs_you":[
            {"id":"1","title":"First","project_label":"Project","task_label":null},
            {"id":"2","title":"Second","project_label":null,"task_label":null},
            {"id":"3","title":"Third","project_label":"Other","task_label":null},
            {"id":"4","title":"Fourth","project_label":null,"task_label":null}],
          "lanes":[{"task_label":"Build menu","project_label":"Project","state":"running","attention":false},
                   {"task_label":null,"project_label":null,"state":"terminal","attention":true}],
          "counts":{"needs_you":4,"running":1,"attention":1},
          "coverage":{"scope":"local","inbox":"partial","runs":"complete","nested_runs":"unavailable"}}}
        """
        let overview = try ConsoleOverview.decode(Data(json.utf8))
        let report = try fixture()
        let menu = QuotaMenu.entries(report: report, error: nil, updatedAt: nil, refreshing: false, lanes: [],
                                     stateRoot: URL(fileURLWithPath: "/tmp"), now: report.generatedAt, overview: overview)
        XCTAssertTrue(menu.contains(.header("At least 4 needs you")))
        let rows = items(menu)
        XCTAssertEqual(Array(rows.prefix(3).map(\.title)), ["First", "Second", "Third"])
        XCTAssertEqual(rows[0].subtitle, "Project")
        XCTAssertNil(rows[1].subtitle)
        XCTAssertEqual(rows.first { $0.title == "Build menu" }?.subtitle, "Project · Running")
        XCTAssertEqual(rows.first { $0.title == "Untitled task" }?.subtitle, "Needs attention")
        XCTAssertTrue(menu.contains(.note("1 more · use provenant inbox", nil)))
        XCTAssertTrue(menu.contains(.note("Local inbox: partial", .orange)))
        XCTAssertLessThan(try XCTUnwrap(rows.firstIndex { $0.title == "First" }),
                          try XCTUnwrap(rows.firstIndex { $0.title == "Claude" }))
    }

    func testInboxCountReflectsCoverageAndNull() throws {
        func heading(_ coverage: String, _ count: Int?) -> String? {
            let overview = ConsoleOverview(needsYou: [], lanes: [],
                counts: .init(needsYou: count, running: 0, attention: 0),
                coverage: .init(scope: "local", inbox: coverage, runs: "complete", nestedRuns: "unavailable"))
            return QuotaMenu.overviewEntries(overview, error: nil, updatedAt: nil, now: Date()).compactMap {
                if case .header(let title) = $0 { title } else { nil }
            }.first
        }
        XCTAssertEqual(heading("complete", 0), "0 needs you")
        XCTAssertEqual(heading("partial", 2), "At least 2 needs you")
        XCTAssertEqual(heading("partial", nil), "Needs you unavailable")
        XCTAssertEqual(heading("unavailable", nil), "Needs you unavailable")
        XCTAssertEqual(heading("incompatible", nil), "Needs you unavailable")
        // An inconsistent response must never make an unavailable source look complete.
        XCTAssertEqual(heading("unavailable", 2), "Needs you unavailable")
        XCTAssertEqual(heading("incompatible", 2), "Needs you unavailable")
    }

    func testSpentAccountsAreFolded() throws {
        let rows = items(entries(try fixture()))
        let spent = try XCTUnwrap(rows.first { $0.title == "Spent accounts" })
        XCTAssertEqual(spent.value, "1")
        XCTAssertEqual(items(try XCTUnwrap(spent.submenu)).map(\.title), ["Antigravity"])
    }

    func testUnavailableOverviewIsNotAnEmptyInbox() throws {
        let menu = QuotaMenu.overviewEntries(nil, error: "failed", updatedAt: nil, now: Date())
        XCTAssertEqual(items(menu).first?.title, "Overview unavailable")
        XCTAssertFalse(menu.contains(.header("0 needs you")))
    }

    func testObservationShortfallAndReadFailureBothRemainVisible() {
        let overview = ConsoleOverview(needsYou: [], lanes: [],
            counts: .init(needsYou: 0, running: 0, attention: 0),
            coverage: .init(scope: "local", inbox: "complete", runs: "complete", nestedRuns: "unavailable"))
        let menu = QuotaMenu.overviewEntries(overview, error: "read failed", updatedAt: Date(timeIntervalSince1970: 0),
                                             now: Date(timeIntervalSince1970: 60), observationError: "Watch limit reached")
        XCTAssertTrue(menu.contains(.header("0 needs you")))
        XCTAssertNotNil(items(menu).first { $0.title == "Overview unavailable" })
        XCTAssertEqual(items(menu).first { $0.title == "Overview observation limited" }?.subtitle, "Watch limit reached")
    }

    // MARK: Display settings

    /// The fixture with display blocks added: `rows` maps an account key to its row display, `global` is the top-level block.
    private func configured(global: [String: Any]? = nil, rows: [String: [String: Any]] = [:]) throws -> QuotaReport {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appending(path: "Fixtures/current.json")
        var json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        var accounts = try XCTUnwrap(json["accounts"] as? [[String: Any]])
        for index in accounts.indices {
            let scope = try XCTUnwrap(accounts[index]["account"] as? [String: Any])
            if let display = rows[scope["account_key"] as? String ?? ""] { accounts[index]["display"] = display }
            // The fixture's windows carry no length; give the Claude pair one so headlines can tell them apart.
            if var snapshot = accounts[index]["snapshot"] as? [String: Any], var windows = snapshot["windows"] as? [[String: Any]] {
                for window in windows.indices {
                    let pool = windows[window]["pool"] as? String ?? ""
                    if pool == "claude-5h" { windows[window]["window_seconds"] = 18_000 }
                    if pool == "claude-weekly" { windows[window]["window_seconds"] = 604_800 }
                }
                snapshot["windows"] = windows
                accounts[index]["snapshot"] = snapshot
            }
        }
        json["accounts"] = accounts
        if let global { json["display"] = global }
        return try CLI.decode(JSONSerialization.data(withJSONObject: json)).get()
    }

    private func titles(_ report: QuotaReport) -> [String] { items(entries(report)).filter { $0.id != nil }.map(\.title) }

    func testOldReportsWithoutDisplayBlocksKeepWorking() throws {
        let report = try fixture()
        XCTAssertEqual(report.display, QuotaReport.Display())
        XCTAssertTrue(report.accounts.allSatisfy { $0.entry == nil })
    }

    func testNameReplacesTitleAndQualifier() throws {
        let report = try configured(rows: ["anese": ["id": "claude-anese", "name": "Work", "hidden": false]])
        let row = try XCTUnwrap(items(entries(report)).first { $0.id == "claude/anese" })
        XCTAssertEqual(row.title, "Work")
        XCTAssertNil(row.detail)
    }

    func testHiddenAccountsAreLeftOut() throws {
        let report = try configured(rows: ["anese": ["id": "claude-anese", "hidden": true]])
        XCTAssertFalse(titles(report).contains("Claude"))
        XCTAssertEqual(titles(report).count, 3)
    }

    func testOrderedAccountsComeFirstAndUnorderedLast() throws {
        let report = try configured(rows: [
            "cursor-test-01": ["id": "cursor", "hidden": false, "order": 1],
            "anese": ["id": "claude-anese", "hidden": false, "order": 2],
        ])
        XCTAssertEqual(titles(report), ["Cursor", "Claude", "Codex", "OpenCode"])
        // A stable sort: equal orders keep the menu's own order.
        let tied = try configured(rows: [
            "cursor-test-01": ["id": "cursor", "hidden": false, "order": 5],
            "anese": ["id": "claude-anese", "hidden": false, "order": 5],
        ])
        XCTAssertEqual(titles(tied).prefix(2), ["Claude", "Cursor"])
    }

    func testHeadlineChoosesTheShortestOrLongestWindowAndAnAccountOverridesTheGlobal() throws {
        let anese = ["id": "claude-anese", "hidden": false] as [String: Any]
        func figure(_ report: QuotaReport) throws -> String? {
            try XCTUnwrap(items(entries(report)).first { $0.id == "claude/anese" }).value
        }
        // Automatic leads with the longest window unless a shorter one is at the alert level.
        XCTAssertEqual(try figure(configured(global: ["headline": "auto", "icon": "most_constrained"])), "52%")
        XCTAssertEqual(try figure(configured(global: ["headline": "5h", "icon": "most_constrained"], rows: ["anese": anese])), "84%")
        let weekly = try configured(global: ["headline": "weekly", "icon": "most_constrained"], rows: ["anese": anese])
        XCTAssertEqual(try figure(weekly), "52%")
        var own = anese
        own["headline"] = "5h"
        let overridden = try configured(global: ["headline": "weekly", "icon": "most_constrained"], rows: ["anese": own])
        XCTAssertEqual(try figure(overridden), "84%")
    }

    func testIconFollowsTheChosenAccountAndMostConstrainedSkipsHidden() throws {
        let now = try fixture().generatedAt
        let base = try configured(rows: ["anese": ["id": "claude-anese", "hidden": false],
                                         "local-test-01": ["id": "opencode", "hidden": false]])
        XCTAssertEqual(base.featured(now: now)?.account.accountKey, "anese")
        let hidden = try configured(rows: ["anese": ["id": "claude-anese", "hidden": true]])
        XCTAssertNotEqual(hidden.featured(now: now)?.account.accountKey, "anese")
        XCTAssertEqual(MenuLabel(report: hidden, failed: false, now: now).spoken.contains("Claude"), false)
        let chosen = try configured(global: ["headline": "auto", "icon": "claude-anese"],
                                    rows: ["anese": ["id": "claude-anese", "hidden": true]])
        XCTAssertEqual(chosen.featured(now: now)?.account.accountKey, "anese")
        XCTAssertEqual(MenuLabel(report: chosen, failed: false, now: now).text, "52%")
    }

    func testSettingsSubmenuRunsTheCliCommands() throws {
        let report = try configured(global: ["headline": "weekly", "icon": "most_constrained"],
                                    rows: ["anese": ["id": "claude-anese", "name": "Work", "hidden": false]])
        let customise = try XCTUnwrap(items(entries(report)).first { $0.title == "Settings" }?.submenu)
        let rows = items(customise)
        XCTAssertEqual(rows.filter(\.checked).map(\.title), ["Weekly", "Most constrained", "Work"])
        XCTAssertTrue(rows.contains { $0.action == .run(["quota", "display", "--headline", "5h"]) })
        XCTAssertTrue(rows.contains { $0.action == .run(["quota", "display", "--icon", "claude-anese"]) })
        XCTAssertTrue(rows.contains { $0.action == .run(["accounts", "set", "claude-anese", "--hidden", "true"]) })
        XCTAssertTrue(rows.contains { $0.title == "Edit Names and Order…" })
    }
}
