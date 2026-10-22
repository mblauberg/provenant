import Foundation

/// `provenant quota --json`, schema provenant.quota.report.v1 (scripts/provider_quota.py).
struct QuotaReport: Decodable {
    static let schemaID = "provenant.quota.report.v1"
    let schema: String
    let host: String?
    let enabled: Bool
    let accounts: [Account]

    struct Account: Decodable, Identifiable {
        var id: String { "\(provider)/\(account)" }
        let provider, account: String
        let plan, error: String?
        let active: Bool?
        let priority: Double?
        let status: String          // ok | stale | unavailable | unreported | disabled
        let observedAt: Date?
        let windows: [Window]
        let unreported: [String]?
        var hasReading: Bool { status == "ok" || status == "stale" }
    }

    struct Window: Decodable, Identifiable {
        var id: String { name }
        let name: String
        let usedPercent: Double?
        let resetAt: Date?
        let pace: Pace
        /// A passed reset makes the reading obsolete until the next fetch.
        func isExpired(_ now: Date) -> Bool { resetAt.map { $0 <= now } ?? false }
    }

    /// exhausted | unknown | unreported | deficit | on_pace | reserve | early | not_started
    struct Pace: Decodable {
        let state: String
        let runsOutAt: Date?
    }

    static func decode(_ data: Data) throws -> QuotaReport {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        // Z or offset forms, with or without fractional seconds; the result is an absolute instant.
        decoder.dateDecodingStrategy = .custom { try Date($0.singleValueContainer().decode(String.self), strategy: .iso8601) }
        return try decoder.decode(QuotaReport.self, from: data)
    }

    /// Accounts in display order: usable, then unreadable, then exhausted; by priority, then report order.
    func sorted(now: Date) -> [Account] {
        accounts.enumerated().sorted { ($0.element.rank(now), $0.element.priority ?? 100, $0.offset)
            < ($1.element.rank(now), $1.element.priority ?? 100, $1.offset) }.map(\.element)
    }

    /// The highest-use binding window across every enabled account (the report lists only those) that has a
    /// current reading and is not exhausted. Exhausted accounts are left out, so a month-long exhaustion does
    /// not pin the headline at 100% and hide the accounts you can still use.
    func top(now: Date) -> (account: Account, window: Window)? {
        accounts.filter { $0.rank(now) == 0 }.compactMap { account in account.binding(now: now).map { (account, $0) } }
            .max { $0.window.usedPercent! < $1.window.usedPercent! }
    }

    func exhausted(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 2 } }

    /// 100% only when no account outside exhaustion has a current reading.
    func headline(now: Date) -> Double? { top(now: now)?.window.usedPercent ?? (exhausted(now: now).isEmpty ? nil : 100) }
}

extension QuotaReport.Account {
    /// The window that binds the account now. A window whose reset has passed no longer counts. An account is
    /// exhausted while an exhausted window still has a future or unknown reset; the one that resets last binds.
    /// Otherwise the current window with the highest use binds.
    func binding(now: Date) -> QuotaReport.Window? {
        let current = windows.filter { $0.usedPercent != nil && !$0.isExpired(now) }
        return current.filter { $0.pace.state == "exhausted" }.max { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
            ?? current.max { $0.usedPercent! < $1.usedPercent! }
    }

    /// 0 usable, 1 no reading, 2 exhausted.
    func rank(_ now: Date) -> Int { !hasReading ? 1 : binding(now: now)?.pace.state == "exhausted" ? 2 : 0 }

    var title: String { account == "default" ? Fmt.human(provider) : "\(Fmt.human(provider)) · \(account)" }
}

enum Fmt {
    private static let span: DateComponentsFormatter = {
        let f = DateComponentsFormatter()
        (f.unitsStyle, f.allowedUnits, f.maximumUnitCount) = (.abbreviated, [.day, .hour, .minute], 2)
        return f
    }()

    /// "5d 8h" from now, or nil once the instant has passed.
    static func until(_ date: Date?, now: Date) -> String? {
        guard let date, date > now else { return nil }
        return span.string(from: max(60, date.timeIntervalSince(now)))
    }

    static func ago(_ date: Date, now: Date) -> String {
        let seconds = now.timeIntervalSince(date)
        return seconds < 60 ? "just now" : "\(span.string(from: seconds) ?? "") ago"
    }

    /// Floored, so 99.6% never reads as exhausted.
    static func percent(_ value: Double) -> String { "\(Int(max(value, 0).rounded(.down)))%" }

    static func human(_ raw: String) -> String {
        let text = raw.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: ":", with: " ")
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    /// "Seven day · runs out in 23h", "Primary · resets in 5d 8h", or "Exhausted · back in 3d 2h".
    static func status(_ window: QuotaReport.Window, now: Date) -> String {
        if window.isExpired(now) { return "\(human(window.name)) · reset passed, awaiting a reading" }
        let reset = until(window.resetAt, now: now)
        if window.pace.state == "exhausted" { return "Exhausted · " + (reset.map { "back in \($0)" } ?? "reset time unknown") }
        if let out = until(window.pace.runsOutAt, now: now) { return "\(human(window.name)) · runs out in \(out)" }
        return human(window.name) + (reset.map { " · resets in \($0)" } ?? "")
    }
}
