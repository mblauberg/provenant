import XCTest
@testable import QuotaMenuBar

final class ReportTests: XCTestCase {
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
                  {"pool": "session:1", "used_basis_points": 300, "resets_unix_seconds": 1899698400, "window_seconds": 18000},
                  {"pool": "weekly_all:2", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "weekly_scoped:Fable", "used_basis_points": 10000, "resets_unix_seconds": 1900285200, "window_seconds": 604800},
                  {"pool": "secondary", "used_basis_points": 10000, "resets_unix_seconds": 1899676800, "window_seconds": 18000}
                ],
                "details": {
                  "plan": "Max 5x", "fingerprint": "0123456789abcdef", "priority": 2,
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
        let report = try QuotaReport.decodeEnvelope(Data(json.utf8), command: "quota show")
        let account = try XCTUnwrap(report.accounts.first)
        XCTAssertEqual(account.planTitle, "Max 5x")
        XCTAssertEqual(account.fingerprint, "0123456789abcdef")
        XCTAssertEqual(account.priority, 2)
        XCTAssertEqual(account.creditBalance, "USD 4.50")
        XCTAssertEqual(account.banked?.available, 2)
        XCTAssertEqual(account.banked?.expiresAt.map(\.timeIntervalSince1970), [1900100000, 1900200000])
        XCTAssertEqual(account.note, "fixed profile")
        let session = try XCTUnwrap(account.windows.first { $0.name == "session:1" })
        XCTAssertEqual(session.title, "Session · 5-hour")
        XCTAssertEqual(session.windowMinutes, 300)
        XCTAssertEqual(session.pace.state, "on_pace")
        let weekly = try XCTUnwrap(account.windows.first { $0.name == "weekly_all:2" })
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
        let exhaustedReport = try QuotaReport.decodeEnvelope(Data(exhaustedGlobal.utf8), command: "quota show")
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
        let report = try QuotaReport.decodeEnvelope(Data(json.utf8), command: "quota show")
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
            currency: nil, advisory: true, pool: "weekly", poolLabel: nil,
            pace: .init(state: "exhausted", runsOutAt: nil, summary: nil))
        XCTAssertTrue(QuotaReport.Account.current([advisory], now: now).isEmpty)
        XCTAssertNil(QuotaReport.Account.worst(QuotaReport.Account.current(
            account.windows.filter { $0.name == "untimed-budget" }, now: now)))
    }
}
