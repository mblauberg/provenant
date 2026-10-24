import XCTest
@testable import QuotaMenuBar

final class ReportTests: XCTestCase {
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
