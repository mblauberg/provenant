import Foundation
import XCTest
@testable import ProvenantKit

final class QuotaPresentationTests: XCTestCase {
    private struct Row: QuotaWindow {
        let name: String
        let pool: String?
        let usedPercent: Double?
        let windowMinutes: Int?
        let resetAt: Date?
        let advisory: Bool?
        let paceState: String
    }

    private func row(_ name: String, _ pool: String?, _ percent: Double, _ minutes: Int,
                     spent: Bool = false, resetAt: Date = Date(timeIntervalSince1970: 1_000)) -> Row {
        Row(name: name, pool: pool, usedPercent: percent, windowMinutes: minutes,
            resetAt: resetAt, advisory: false,
            paceState: spent || percent >= 100 ? "exhausted" : "on_pace")
    }

    func testPooledWindowNamesAreHumanAndKeepTheirOrdinal() {
        let cases: [(String, String, UInt64?, String)] = [
            ("agy", "gemini.weekly", 604_800, "Gemini weekly"),
            ("agy", "gemini.5h", 18_000, "Gemini 5-hour"),
            ("agy", "claude.weekly", 604_800, "Claude weekly"),
            ("agy", "gemini.weekly:2", 604_800, "Gemini weekly (2)"),
            ("claude", "seven_day", 604_800, "Weekly"),
        ]
        for (pool, name, seconds, expected) in cases {
            let presentation = QuotaPresentation.presentation(adapter: pool, name: name, seconds: seconds)
            XCTAssertEqual(QuotaPresentation.windowName(presentation.title, poolLabel: presentation.poolLabel), expected)
        }
    }

    func testAutomaticHeadlineIgnoresAnExhaustedPoolOfAnotherProvider() {
        let now = Date(timeIntervalSince1970: 500)
        let windows = [
            row("gemini.weekly", "gemini", 49.68, 10_080),
            row("gemini.5h", "gemini", 0, 300),
            row("gemini.weekly:2", "gemini", 6.46, 10_080),
            row("claude.weekly", "claude", 100, 10_080, spent: true),
        ]
        let headline = QuotaPresentation.headline(windows, now: now, alert: QuotaPresentation.alertPercent, choice: .auto)
        XCTAssertEqual(headline?.name, "gemini.weekly")
        XCTAssertEqual(Int((headline?.usedPercent ?? 0).rounded(.down)), 49)
    }

    func testHeadlineNeverLeadsWithAWindowThatHasAlreadyReset() {
        let now = Date(timeIntervalSince1970: 500)
        let expired = row("weekly", nil, 100, 10_080, spent: true, resetAt: Date(timeIntervalSince1970: 400))
        let current = row("five_hour", nil, 50, 300)
        let windows = [expired, current]
        for choice in [QuotaPresentation.Headline.auto, .fiveHour, .weekly] {
            XCTAssertEqual(QuotaPresentation.headline(windows, now: now, alert: 90, choice: choice)?.name,
                           "five_hour", "headline \(choice)")
        }
    }

    func testHeadlineSkipsAnExpiredSpentWindowInTheBindingPool() {
        let now = Date(timeIntervalSince1970: 500)
        let expired = row("gpt.secondary", "gpt", 100, 10_080, spent: true, resetAt: Date(timeIntervalSince1970: 400))
        let current = row("gpt.primary", "gpt", 50, 600)
        let headline = QuotaPresentation.headline([expired, current], now: now, alert: 90, choice: .auto)
        XCTAssertEqual(headline?.name, "gpt.primary")
        XCTAssertEqual(headline?.usedPercent, 50)
    }

    func testChosenHeadlinePicksTheShortestOrLongestWindowInTheBindingPool() {
        let now = Date(timeIntervalSince1970: 500)
        let windows = [
            row("claude-5h", nil, 84, 300),
            row("seven_day", nil, 52, 10_080),
        ]
        XCTAssertEqual(QuotaPresentation.headline(windows, now: now, alert: 90, choice: .fiveHour)?.name, "claude-5h")
        XCTAssertEqual(QuotaPresentation.headline(windows, now: now, alert: 90, choice: .weekly)?.name, "seven_day")
    }
}
