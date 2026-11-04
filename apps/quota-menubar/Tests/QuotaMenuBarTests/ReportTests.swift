import XCTest
import ProvenantKit
@testable import QuotaMenuBar

final class ReportTests: XCTestCase {
    /// A `quota show` envelope, decoded through the same path the menu uses.
    private func decodeShow(_ json: String) throws -> QuotaReport {
        let run = CLI.Output(status: 0, signalled: false, timedOut: false, truncated: false, out: Data(json.utf8), err: "")
        return try QuotaReport.project(CLIClient.decode(run, command: "quota show", as: QuotaWireReport.self))
    }

    @MainActor func testPartialCoverageAppearsInMenuRow() throws {
        let json = #"""
        {"schema_version":1,"command":"quota show","status":"ok","error":null,"data":{
         "schema_version":1,"generated_unix_seconds":1899680400,
         "accounts":[{"account":{"adapter":"codex","account_key":"a","label":"A"},"status":"available","refreshing":false,
         "snapshot":{"windows":[{"pool":"primary","used_basis_points":2400,"resets_unix_seconds":1899698400}],"details":{"coverage":"partial"}}}],
         "sources":[],"coverage":{"total":1,"known":1},"refreshing_ids":[],"report_path":"/tmp/report"}}
        """#
        let report = try decodeShow(json)
        let account = try XCTUnwrap(report.accounts.first)
        let row = QuotaMenu.account(account, now: report.generatedAt, alert: report.alertPercent)
        XCTAssertEqual(row.value, "≥24%")
        XCTAssertTrue(MenuStyle.spoken(row).contains("at least 24%"))
    }

    func testPanelEventIsRetainedDuringRefresh() {
        XCTAssertEqual(Trigger.panel.queued(after: nil), .panel)
        XCTAssertEqual(Trigger.panel.queued(after: .power), .panel)
        XCTAssertEqual(Trigger.timer.queued(after: .panel), .panel)
        XCTAssertEqual(Trigger.power.queued(after: .panel), .panel)
        XCTAssertEqual(Trigger.panel.queued(after: .wake), .wake)
        XCTAssertEqual(Trigger.manual.queued(after: .panel), .manual)
        XCTAssertEqual(Trigger.panel.queued(after: .manual), .manual)
        XCTAssertNil(Trigger.timer.queued(after: nil))
    }

    func testSnapshotDetailsFromRustEnvelope() throws {
        let json = #"""
        {
          "schema_version": 1, "command": "quota show", "status": "ok", "error": null,
          "data": {
            "schema_version": 1, "generated_unix_seconds": 1899680400,
            "accounts": [{
              "account": {"adapter": "claude", "account_key": "default", "label": "Pro"},
              "snapshot": {
                "schema_version": 1,
                "windows": [
                  {"pool": "five_hour", "used_basis_points": 300, "resets_unix_seconds": 1899698400, "window_seconds": 18000},
                  {"pool": "seven_day", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "weekly_scoped:Fable", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "secondary", "used_basis_points": 10000, "resets_unix_seconds": 1899676800, "window_seconds": 18000}
                ],
                "details": {
                  "coverage": "partial", "plan": "Max 5x", "fingerprint": "0123456789abcdef", "priority": 2,
                  "credit_balance": "USD 4.50", "reset_credits": 2,
                  "reset_expiries_unix_seconds": [1900200000, 1900100000], "note": "fixed profile"
                }
              },
              "status": "available", "age_seconds": 0, "next_due_unix_seconds": null,
              "refreshing": false, "error": null
            }, {
              "account": {"adapter": "agy", "account_key": "test", "label": "test"},
              "snapshot": {
                "schema_version": 1,
                "windows": [
                  {"pool": "gemini.weekly", "used_basis_points": 265, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "gemini.5h", "used_basis_points": 927, "resets_unix_seconds": 1899698400, "window_seconds": 18000},
                  {"pool": "claude.weekly", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800}
                ]
              },
              "status": "available", "age_seconds": 0, "next_due_unix_seconds": null,
              "refreshing": false, "error": null
            }, {
              "account": {"adapter": "codex", "account_key": "test", "label": "test"},
              "snapshot": {
                "schema_version": 1,
                "windows": [
                  {"pool": "primary", "used_basis_points": 5000, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "secondary", "used_basis_points": 2000, "resets_unix_seconds": 1899698400, "window_seconds": 18000},
                  {"pool": "gpt-5-codex.primary", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800}
                ]
              },
              "status": "available", "age_seconds": 0, "next_due_unix_seconds": null,
              "refreshing": false, "error": null
            }],
            "sources": [], "coverage": {"total": 3, "known": 3},
            "refreshing_ids": [], "next_due_unix_seconds": null,
            "report_path": "/tmp/provenant/quota/report.json"
          }
        }
        """#
        let report = try decodeShow(json)
        let account = try XCTUnwrap(report.accounts.first)
        XCTAssertEqual(account.planTitle, "Max 5x")
        XCTAssertEqual(account.fingerprint, "0123456789abcdef")
        XCTAssertEqual(account.priority, 2)
        XCTAssertEqual(account.creditBalance, "USD 4.50")
        XCTAssertEqual(account.banked?.available, 2)
        XCTAssertEqual(account.banked?.expiresAt.map(\.timeIntervalSince1970), [1900100000, 1900200000])
        XCTAssertEqual(account.note, "fixed profile")
        let session = try XCTUnwrap(account.windows.first { $0.name == "five_hour" })
        XCTAssertTrue(session.lowerBound)
        XCTAssertEqual(Fmt.used(session), "≥3%")
        XCTAssertEqual(session.title, "5-hour")
        XCTAssertEqual(session.windowMinutes, 300)
        XCTAssertEqual(session.pace.state, "on_pace")
        let weekly = try XCTUnwrap(account.windows.first { $0.name == "seven_day" })
        XCTAssertEqual(weekly.title, "Weekly")
        XCTAssertEqual(weekly.windowMinutes, 10_080)
        XCTAssertEqual(weekly.pace.state, "exhausted")
        let scoped = try XCTUnwrap(account.windows.first { $0.name == "weekly_scoped:Fable" })
        XCTAssertEqual(scoped.title, "Weekly · Fable")
        XCTAssertTrue(scoped.advisory == true)
        let expired = try XCTUnwrap(account.windows.first { $0.name == "secondary" })
        XCTAssertTrue(expired.isExpired(report.generatedAt))
        XCTAssertFalse(QuotaReport.Account.current(account.windows, now: report.generatedAt).contains { $0.id == expired.id })

        let agy = try XCTUnwrap(report.accounts.first { $0.provider == "agy" })
        let gemini = try XCTUnwrap(agy.windows.first { $0.name == "gemini.weekly" })
        let geminiShort = try XCTUnwrap(agy.windows.first { $0.name == "gemini.5h" })
        let claude = try XCTUnwrap(agy.windows.first { $0.name == "claude.weekly" })
        XCTAssertEqual(gemini.fullTitle, "Gemini · Weekly")
        XCTAssertEqual(gemini.pool, geminiShort.pool)
        XCTAssertEqual(gemini.windowMinutes, 10_080)
        XCTAssertEqual(geminiShort.windowMinutes, 300)
        XCTAssertEqual(claude.fullTitle, "Claude · Weekly")
        XCTAssertEqual(claude.pace.state, "exhausted")
        XCTAssertEqual(agy.binding(now: report.generatedAt)?.pool, "gemini")

        let codex = try XCTUnwrap(report.accounts.first { $0.provider == "codex" })
        let modelWindow = try XCTUnwrap(codex.windows.first { $0.name == "gpt-5-codex.primary" })
        XCTAssertEqual(modelWindow.title, "Weekly")
        XCTAssertEqual(modelWindow.pool, "gpt-5-codex")
        XCTAssertEqual(modelWindow.poolLabel, "GPT 5 Codex")
        XCTAssertEqual(modelWindow.fullTitle, "GPT 5 Codex · Weekly")
        XCTAssertEqual(modelWindow.pace.state, "exhausted")
        XCTAssertEqual(codex.binding(now: report.generatedAt)?.name, "primary")
        XCTAssertEqual(codex.binding(now: report.generatedAt)?.pace.state, "on_pace")
        XCTAssertEqual(codex.poolRings(now: report.generatedAt, alert: report.alertPercent).count, 2)
        let exhaustedGlobal = json
            .replacingOccurrences(of: #""pool": "primary", "used_basis_points": 5000"#, with: #""pool": "primary", "used_basis_points": 10000"#)
            .replacingOccurrences(of: #""pool": "gpt-5-codex.primary", "used_basis_points": 10000"#, with: #""pool": "gpt-5-codex.primary", "used_basis_points": 0"#)
        let exhaustedReport = try decodeShow(exhaustedGlobal)
        let exhaustedCodex = try XCTUnwrap(exhaustedReport.accounts.first { $0.provider == "codex" })
        XCTAssertEqual(exhaustedCodex.binding(now: exhaustedReport.generatedAt)?.pace.state, "exhausted")

    }

    func testBindingAndCreditDrawdownFromRustEnvelope() throws {
        // The versioned quota show envelope and snapshot fields owned by the Rust CLI.
        let json = #"""
        {
          "schema_version": 1, "command": "quota show", "status": "ok", "error": null,
          "data": {
            "schema_version": 1, "generated_unix_seconds": 1899680400,
            "accounts": [{
              "account": {"adapter": "codex", "account_key": "default", "label": "default"},
              "snapshot": {
                "schema_version": 1,
                "account": {"adapter": "codex", "account_key": "default", "label": "default"},
                "revision": 1, "status": "credit_only", "observed_unix_seconds": 1899680400,
                "source_ref": "fixture", "authority_ref": null,
                "windows": [
                  {"pool": "untimed-budget", "used_basis_points": 10000, "resets_unix_seconds": null},
                  {"pool": "weekly", "used_basis_points": 10000, "resets_unix_seconds": 1900198800}
                ]
              },
              "status": "credit_only", "age_seconds": 0, "next_due_unix_seconds": null,
              "refreshing": false, "error": null
            }],
            "sources": [], "coverage": {"total": 1, "known": 1},
            "refreshing_ids": [], "next_due_unix_seconds": null,
            "report_path": "/tmp/provenant/quota/report.json"
          }
        }
        """#
        let report = try decodeShow(json)
        let account = try XCTUnwrap(report.accounts.first)
        let now = report.generatedAt
        XCTAssertEqual(account.binding(now: now)?.name, "weekly")
        let row = Fmt.row(account, now: now, alert: report.alertPercent)
        XCTAssertTrue(row.lead?.contains("Exhausted, drawing on credits") == true)
        XCTAssertTrue(row.spoken.contains("exhausted, drawing on credits"))
        XCTAssertTrue(row.lead?.contains("Allowance back") == true)

        // Advisory is a Swift projection flag, absent from the current Rust wire format.
        let advisory = QuotaReport.Window(
            name: "advisory", label: nil, usedPercent: 100, windowMinutes: 60,
            resetAt: now.addingTimeInterval(3600), usedAmount: nil, limitAmount: nil,
            currency: nil, advisory: true, pool: "weekly", poolLabel: nil, lowerBound: false,
            pace: .init(state: "exhausted", runsOutAt: nil, summary: nil))
        XCTAssertTrue(QuotaReport.Account.current([advisory], now: now).isEmpty)
        XCTAssertNil(QuotaReport.Account.worst(QuotaReport.Account.current(
            account.windows.filter { $0.name == "untimed-budget" }, now: now)))
    }

    private func showJSON(accountStatus: String, sourceStatus: String = "unknown", repair: String? = nil) -> String {
        let repairField = repair.map { #""repair": "\#($0)","# } ?? ""
        return #"""
        {
          "schema_version": 1, "command": "quota show", "status": "ok", "error": null,
          "data": {
            "schema_version": 1, "generated_unix_seconds": 1793577600,
            "accounts": [{
              "account": {"adapter": "agy", "account_key": "agy-google-abc", "label": "legacy"},
              "snapshot": null, "status": "\#(accountStatus)", \#(repairField)
              "age_seconds": null, "next_due_unix_seconds": null, "refreshing": false,
              "error": "isolated_keychain_unavailable"
            }, {
              "account": {"adapter": "codex", "account_key": "ok", "label": "ok"},
              "snapshot": null, "status": "unknown",
              "age_seconds": null, "next_due_unix_seconds": null, "refreshing": false, "error": null
            }],
            "sources": [{"adapter": "agy", "status": "\#(sourceStatus)", "error": null, "next_due_unix_seconds": null}],
            "coverage": {"total": 2, "known": 0},
            "refreshing_ids": [], "next_due_unix_seconds": null,
            "report_path": "/tmp/provenant/quota/report.json"
          }
        }
        """#
    }

    func testKeychainUnavailableDecodesAsNeedingActionWithRepairText() throws {
        let repair = "expected /tmp/.gemini/Library/Keychains/login.keychain-db; re-register with `provenant accounts register --adapter agy --directory ~ legacy`"
        let report = try decodeShow(showJSON(accountStatus: "keychain_unavailable", sourceStatus: "keychain_unavailable", repair: repair))
        let account = try XCTUnwrap(report.accounts.first { $0.provider == "agy" })
        XCTAssertEqual(account.status, .keychainUnavailable)
        XCTAssertTrue(account.needsAction)
        XCTAssertEqual(account.repair, repair)
        XCTAssertEqual(account.statusTitle, "Keychain unavailable")
        XCTAssertEqual(report.sources.first?.status, .keychainUnavailable)
        XCTAssertEqual(account.rank(report.generatedAt), 4)
        let row = QuotaMenu.account(account, now: report.generatedAt, alert: report.alertPercent)
        XCTAssertEqual(row.subtitle, "Keychain unavailable")
        let line = Fmt.row(account, now: report.generatedAt, alert: report.alertPercent)
        XCTAssertEqual(line.lead, "Keychain unavailable")
        XCTAssertTrue(line.spoken.contains(repair))
    }

    func testUnknownFutureStatusDegradesOneRowInsteadOfFailingTheReport() throws {
        let report = try decodeShow(showJSON(accountStatus: "some_future_status", sourceStatus: "some_future_status"))
        XCTAssertEqual(report.accounts.count, 2)
        let account = try XCTUnwrap(report.accounts.first { $0.provider == "agy" })
        XCTAssertEqual(account.status, .unknown)
        XCTAssertNil(account.repair)
        XCTAssertEqual(report.sources.first?.status, .unknown)
        XCTAssertEqual(report.accounts.last?.status, .unknown)
    }

    func testRefusalResetDecodesAndAppearsOnTheMenuBarRow() throws {
        let json = #"""
        {
          "schema_version": 1, "command": "quota show", "status": "ok", "error": null,
          "data": {
            "schema_version": 1, "generated_unix_seconds": 1793577600,
            "accounts": [{
              "account": {"adapter": "opencode", "account_key": "zen", "label": "zen"},
              "snapshot": null, "status": "exhausted", "resets_at": "2026-11-02T00:11:00Z",
              "free_pool_refusal": true,
              "refusals": [{"model": "opencode/beam-free", "pool": "opencode-free",
                "resets_at": "2026-11-02T00:11:00Z"}],
              "age_seconds": null, "next_due_unix_seconds": null, "refreshing": false, "error": null
            }, {
              "account": {"adapter": "opencode", "account_key": "no-reset", "label": "no-reset"},
              "snapshot": {"windows": [{"pool": "opencode-free", "used_basis_points": 10000,
                "resets_unix_seconds": 1793581200}]},
              "status": "exhausted", "free_pool_refusal": true,
              "refusals": [{"model": "opencode/beam-free", "pool": "opencode-free"}],
              "age_seconds": null, "next_due_unix_seconds": null, "refreshing": false, "error": null
            }],
            "sources": [], "coverage": {"total": 1, "known": 0},
            "refreshing_ids": [], "next_due_unix_seconds": null,
            "report_path": "/tmp/provenant/quota/report.json"
          }
        }
        """#
        let report = try decodeShow(json)
        let account = try XCTUnwrap(report.accounts.first)
        XCTAssertEqual(account.backAt, try Date("2026-11-02T00:11:00Z", strategy: .iso8601))
        let row = QuotaMenu.account(account, now: report.generatedAt, alert: report.alertPercent)
        XCTAssertTrue(row.subtitle?.contains("beam-free refused") == true)
        let expiry = Fmt.moment(try Date("2026-11-02T00:11:00Z", strategy: .iso8601), now: report.generatedAt)
        XCTAssertTrue(row.subtitle?.contains("until \(expiry)") == true, row.subtitle ?? "")
        let noReset = try XCTUnwrap(report.accounts.last)
        XCTAssertNil(noReset.backAt)
        XCTAssertTrue(QuotaMenu.account(noReset, now: report.generatedAt, alert: report.alertPercent)
            .subtitle?.contains("beam-free refused") == true)
        XCTAssertFalse(QuotaMenu.account(noReset, now: report.generatedAt, alert: report.alertPercent)
            .subtitle?.contains("until") == true)
    }

    @MainActor
    func testRefusalsStayShortInTheRowAndListInFullInTheSubmenu() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let soon = now.addingTimeInterval(3_600), later = now.addingTimeInterval(7_200)
        let refusals = [QuotaReport.Refusal(model: "agy/claude-opus-4-6-thinking", pool: nil, resetsAt: later),
                        QuotaReport.Refusal(model: "agy/claude-sonnet-4-6", pool: "claude", resetsAt: soon)]
        XCTAssertEqual(QuotaMenu.refusalSummary(refusals, now: now), "2 models refused until \(Fmt.moment(soon, now: now))")
        XCTAssertEqual(QuotaMenu.refusalSummary(Array(refusals.prefix(1)), now: now),
                       "claude-opus-4-6-thinking refused until \(Fmt.moment(later, now: now))")
        XCTAssertNil(QuotaMenu.refusalSummary([], now: now))
        XCTAssertTrue(QuotaMenu.refusalLine(refusals[1], now: now).hasPrefix("Refused agy/claude-sonnet-4-6 in pool claude"))
    }

    @MainActor
    func testFreeTextCannotWidenTheMenuPastTheMaximumColumn() {
        let long = String(repeating: "Individual quota reached ", count: 12)
        let spec = MenuItemSpec(title: "Antigravity", subtitle: long, icon: .ring(nil, .secondary))
        XCTAssertEqual(MenuStyle.column(for: [spec]), max(MenuStyle.minimumColumn, MenuStyle.maximumColumn))
    }
}
