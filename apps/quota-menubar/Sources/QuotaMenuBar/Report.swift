import Foundation

/// `provenant quota --json`, schema provenant.quota.report.v1 (scripts/provider_quota.py).
struct QuotaReport: Decodable {
    static let schemaID = "provenant.quota.report.v1"
    let schema: String
    let host: String?
    let enabled: Bool
    let accounts: [Account]
    // From `provenant quota tick`; an older CLI leaves them out.
    let generatedAt: Date?
    let nextDueAt: Date?
    private let refreshing: Lenient<[String]>?

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
        let drawingCredits: Bool?   // the allowance is spent and credits keep the account usable
        let creditBurnPerHour: Double?
        private let schedule: Lenient<Schedule>?, usageCredits: Lenient<[Credit]>?, resets: Lenient<Resets>?
        var hasReading: Bool { status == "ok" || status == "stale" }
        var next: Schedule? { schedule?.value }
        var credits: [Credit] { usageCredits?.value ?? [] }
        var banked: Resets? { resets?.value }
    }

    struct Schedule: Decodable {
        let nextDueAt: Date
        let reason: String?
    }

    /// A credit balance; `balance` is nil when unlimited.
    struct Credit: Decodable {
        let balance: Double?
        let unlimited: Bool
        let currency, unit: String?
        let expiresAt: Date?
    }

    /// Banked resets available now, with expiries soonest first.
    struct Resets: Decodable {
        let available: Int
        let expiresAt: [Date]
    }

    struct Window: Decodable, Identifiable {
        var id: String { name }
        let name: String
        let label: String?
        let usedPercent: Double?
        let windowMinutes: Int?
        let resetAt: Date?
        let usedAmount, limitAmount: Double?   // a budget window, such as dollars of cloud session credit
        let currency: String?
        let advisory: Bool?
        let pace: Pace
        var title: String { label ?? Fmt.human(name) }
        /// As in provider_quota.py `sets_state`: an untimed budget or an advisory window never binds the account.
        var setsState: Bool { advisory != true && !(limitAmount != nil && (windowMinutes ?? 0) == 0) }
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

    /// A tick names the accounts it is fetching; a list older than the CLI's run limit is a crashed run's leftover.
    func isRefreshing(now: Date) -> Bool {
        !(refreshing?.value ?? []).isEmpty && generatedAt.map { now.timeIntervalSince($0) < 60 } ?? false
    }

    /// The account schedule due soonest.
    var nextCheck: Schedule? { accounts.compactMap(\.next).min { $0.nextDueAt < $1.nextDueAt } }

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

    func exhausted(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 3 } }

    func drawing(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 1 } }

    /// 100% only when no account outside exhaustion or credit use has a current reading.
    func headline(now: Date) -> Double? {
        top(now: now)?.window.usedPercent ?? (exhausted(now: now).isEmpty && drawing(now: now).isEmpty ? nil : 100)
    }
}

/// Decodes to nil instead of failing the whole report when an optional block has an unexpected shape.
struct Lenient<Value: Decodable>: Decodable {
    let value: Value?
    init(from decoder: Decoder) throws { value = try? Value(from: decoder) }
}

extension QuotaReport.Account {
    /// The window that binds the account now. A window whose reset has passed no longer counts. An account is
    /// exhausted while an exhausted window still has a future or unknown reset; the one that resets last binds.
    /// Otherwise the current window with the highest use binds.
    func binding(now: Date) -> QuotaReport.Window? {
        let current = windows.filter { $0.usedPercent != nil && !$0.isExpired(now) && $0.setsState }
        return current.filter { $0.pace.state == "exhausted" }.max { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
            ?? current.max { $0.usedPercent! < $1.usedPercent! }
    }

    /// Drawing on credits, per the report, until an exhausted window behind it resets.
    func isDrawing(now: Date) -> Bool {
        drawingCredits == true && (binding(now: now)?.pace.state == "exhausted" || !windows.contains { $0.pace.state == "exhausted" })
    }

    /// 0 usable, 1 drawing on credits, 2 no reading, 3 exhausted.
    func rank(_ now: Date) -> Int {
        !hasReading ? 2 : isDrawing(now: now) ? 1 : binding(now: now)?.pace.state == "exhausted" ? 3 : 0
    }

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

    /// "US$136" or "$136" by locale; cents only when there are some.
    static func money(_ value: Double, _ currency: String?) -> String {
        let digits = value == value.rounded() ? 0 : 2
        return currency.map { value.formatted(.currency(code: $0).precision(.fractionLength(digits))) }
            ?? value.formatted(.number.precision(.fractionLength(digits)))
    }

    /// "$136 of $250" for a budget window, otherwise the percentage.
    static func used(_ window: QuotaReport.Window) -> String {
        guard let used = window.usedAmount, let limit = window.limitAmount else { return percent(window.usedPercent ?? 0) }
        return "\(money(used, window.currency)) of \(money(limit, window.currency))"
    }

    static func day(_ date: Date) -> String { date.formatted(.dateTime.day().month()) }

    /// "US$42 credit available · expires 3 Nov · about US$5/h".
    static func credit(_ entry: QuotaReport.Credit, burn: Double?) -> String {
        let amount = entry.unlimited ? "Unlimited credits"
            : entry.currency != nil ? "\(money(entry.balance ?? 0, entry.currency)) credit"
            : "\(money(entry.balance ?? 0, nil)) \(entry.unit.map(human) ?? "credits")"
        return [amount + " available", entry.expiresAt.map { "expires \(day($0))" },
                burn.map { "about \(money($0.rounded(), entry.currency))/h" }].compactMap { $0 }.joined(separator: " · ")
    }

    /// "2 banked resets · soonest expires 3 Nov".
    static func resets(_ resets: QuotaReport.Resets) -> String {
        let lead = resets.expiresAt.count > 1 ? "soonest expires" : "expires"
        return "\(resets.available) banked reset\(resets.available == 1 ? "" : "s")"
            + (resets.expiresAt.first.map { " · \(lead) \(day($0))" } ?? "")
    }

    /// "Next check in 4m · active".
    static func next(_ schedule: QuotaReport.Schedule, now: Date) -> String {
        (until(schedule.nextDueAt, now: now).map { "Next check in \($0)" } ?? "Next check due now")
            + (schedule.reason.map { " · " + $0.replacingOccurrences(of: "_", with: " ") } ?? "")
    }

    static func human(_ raw: String) -> String {
        let text = raw.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: ":", with: " ")
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    /// "Seven day · runs out in 23h", "Primary · resets in 5d 8h", or "Exhausted · back in 3d 2h".
    static func status(_ window: QuotaReport.Window, now: Date) -> String {
        if window.isExpired(now) { return "\(window.title) · reset passed, awaiting a reading" }
        let reset = until(window.resetAt, now: now)
        if window.pace.state == "exhausted" { return "Exhausted · " + (reset.map { "back in \($0)" } ?? "reset time unknown") }
        if let out = until(window.pace.runsOutAt, now: now) { return "\(window.title) · runs out in \(out)" }
        return window.title + (reset.map { " · resets in \($0)" } ?? "")
    }

    /// "Drawing on credits · allowance back in 3d 2h" for an account whose allowance is spent.
    static func drawing(_ window: QuotaReport.Window?, now: Date) -> String {
        let back = window?.pace.state == "exhausted" ? until(window?.resetAt, now: now) : nil
        return "Drawing on credits" + (back.map { " · allowance back in \($0)" } ?? "")
    }
}
