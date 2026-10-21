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
        let status: String          // ok | stale | unavailable | unreported | disabled
        let state: String           // worst window pace state, or "unreported"
        let observedAt: Date?
        let windows: [Window]
        let unreported: [String]?
        var hasReading: Bool { status == "ok" || status == "stale" }
        /// Exhausted only while an exhausted window's reset is still ahead (or unknown).
        func isExhausted(now: Date) -> Bool {
            windows.contains { $0.pace.state == "exhausted" && ($0.resetAt.map { $0 > now } ?? true) }
        }
    }

    struct Window: Decodable, Identifiable {
        var id: String { name }
        let name: String
        let usedPercent, windowMinutes: Double?
        let resetAt: Date?
        let pace: Pace
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

    func exhausted(now: Date) -> [Account] { accounts.filter { $0.hasReading && $0.isExhausted(now: now) } }
    var unavailable: [Account] { accounts.filter { !$0.hasReading } }

    /// Highest used % across every enabled account (the report lists only those) that has a current
    /// reading. Exhausted accounts are left out, so a month-long exhaustion does not pin the headline at
    /// 100% and hide the accounts you can still use, and a window whose reset has passed no longer counts.
    /// It reads 100% only when no account outside exhaustion has a current reading.
    func headline(now: Date) -> Double? {
        let used = accounts.filter { $0.hasReading && !$0.isExhausted(now: now) }.flatMap(\.windows)
            .filter { ($0.resetAt ?? .distantFuture) > now }.compactMap(\.usedPercent)
        return used.max() ?? (exhausted(now: now).isEmpty ? nil : 100)
    }

    var byProvider: [(provider: String, accounts: [Account])] {
        var seen: [String] = []
        for account in accounts where !seen.contains(account.provider) { seen.append(account.provider) }
        return seen.map { name in (name, accounts.filter { $0.provider == name }) }
    }
}

enum Fmt {
    private static func formatter(_ style: DateComponentsFormatter.UnitsStyle, units: Int) -> DateComponentsFormatter {
        let f = DateComponentsFormatter()
        f.unitsStyle = style
        f.allowedUnits = [.day, .hour, .minute]
        f.maximumUnitCount = units
        return f
    }
    private static let span = formatter(.abbreviated, units: 2), length = formatter(.full, units: 1)

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

    static func windowTitle(_ window: QuotaReport.Window) -> String {
        // Skip the length when the name already states it ("five_hour").
        guard let minutes = window.windowMinutes, minutes > 0, let span = length.string(from: minutes * 60),
              !["hour", "day", "week", "month"].contains(where: window.name.contains) else { return human(window.name) }
        return "\(human(window.name)) · \(span)"
    }

    private static let paceLabels = ["deficit": "Over pace", "reserve": "Under pace", "on_pace": "On pace",
                                     "early": "Just started", "not_started": "Starts on first use",
                                     "exhausted": "Exhausted", "unreported": "Not reported"]
    static func paceLabel(_ state: String) -> String { paceLabels[state] ?? "Pace unknown" }
}
