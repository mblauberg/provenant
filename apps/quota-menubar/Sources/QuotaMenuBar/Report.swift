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
    // Display settings and the menu bar's account; an older CLI leaves them out.
    private let display: Lenient<Display>?, headline: Lenient<Headline>?
    var alertPercent: Double { display?.value?.shortWindowAlertPercent ?? 90 }
    var pick: Headline? { headline?.value }

    /// `quota.display`: a shorter window shows instead of the longest once it reaches this use.
    struct Display: Decodable { let shortWindowAlertPercent: Double? }

    /// `quota.menubar.headline`: recent | worst | none | <provider>/<account>, and the account it chose.
    struct Headline: Decodable {
        let mode: String
        let account: String?
    }

    struct Account: Decodable, Identifiable {
        var id: String { "\(provider)/\(account)" }
        let provider, account: String
        let plan, error: String?
        let active: Bool?
        let priority: Double?
        let status: String          // ok | stale | unavailable | unreported | disabled | signed_out
        // The CLI's display vocabulary; an older CLI leaves them out.
        let providerName, displayName, planLabel: String?
        let backAt: Date?           // when an exhausted or signed-out account's first pool comes back
        let observedAt: Date?
        let windows: [Window]
        let unreported: [String]?
        let drawingCredits: Bool?   // the allowance is spent and credits keep the account usable
        let creditBurnPerHour: Double?
        private let schedule: Lenient<Schedule>?, usageCredits: Lenient<[Credit]>?, resets: Lenient<Resets>?
        var hasReading: Bool { status == "ok" || status == "stale" }
        /// Signed out of a single-account credential store: the last reading is kept, and never fetched.
        var isSignedOut: Bool { status == "signed_out" }
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
        let pool, poolLabel: String?   // windows of one pool bind only that pool, such as agy's Gemini models
        let pace: Pace
        var title: String { label ?? Fmt.human(name) }
        /// "Claude models · Weekly" for a pooled window.
        var fullTitle: String { poolLabel.map { "\($0) · \(title)" } ?? title }
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

    /// Accounts in display order: usable, credits, unreadable, exhausted, signed out; by priority, then report order.
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

    /// The account the menu bar shows and the window to show for it. The CLI picks it (`quota.menubar.headline`);
    /// an account it picked that is no longer usable gives way to the highest use, unless it was pinned. An older
    /// CLI picks nothing, so the highest use shows. Nil for `none` or when nothing usable remains.
    func featured(now: Date) -> (account: Account, window: Window)? {
        guard let pick else { return top(now: now).map { ($0.account, $0.account.display(now: now, alert: alertPercent) ?? $0.window) } }
        guard let id = pick.account, let account = accounts.first(where: { $0.id == id }) else { return nil }
        let pinned = pick.mode.contains("/")
        guard pinned || account.rank(now) == 0 else { return top(now: now).flatMap { featured($0.account, now) } }
        return featured(account, now)
    }

    private func featured(_ account: Account, _ now: Date) -> (account: Account, window: Window)? {
        account.display(now: now, alert: alertPercent).map { (account, $0) }
    }

    func exhausted(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 3 } }

    /// The menu bar shows the gauge alone: `none`, or the CLI found nothing usable.
    var iconOnly: Bool { pick.map { $0.mode == "none" || $0.account == nil } ?? false }

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
    typealias Window = QuotaReport.Window

    /// Pool names in report order; empty when nothing is pooled.
    var pools: [String] { windows.compactMap(\.pool).reduce(into: []) { if !$0.contains($1) { $0.append($1) } } }

    /// A pool's windows with the unpooled ones that bind every pool.
    func windows(in pool: String?) -> [Window] { windows.filter { $0.pool == nil || $0.pool == pool } }

    /// Windows with a reading that still holds: a window whose reset has passed no longer counts.
    static func current(_ windows: [Window], now: Date) -> [Window] {
        windows.filter { $0.usedPercent != nil && !$0.isExpired(now) && $0.setsState }
    }

    /// The window that constrains these most: an exhausted one, the one that resets last; else the highest use.
    static func worst(_ windows: [Window]) -> Window? {
        windows.filter { $0.pace.state == "exhausted" }.max { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
            ?? windows.max { $0.usedPercent! < $1.usedPercent! }
    }

    /// As in provider_quota.py `display`: an exhausted window; else a shorter window at the alert use or more,
    /// even when the longest is higher, since the short one runs out first; else the longest.
    static func shown(_ windows: [Window], alert: Double) -> Window? {
        if let spent = worst(windows), spent.pace.state == "exhausted" { return spent }
        let longest = windows.max { ($0.windowMinutes ?? 0, $0.usedPercent!) < ($1.windowMinutes ?? 0, $1.usedPercent!) }
        let alerts = windows.filter { $0.usedPercent! >= alert && ($0.windowMinutes ?? 0) < (longest?.windowMinutes ?? 0) }
        return alerts.max { $0.usedPercent! < $1.usedPercent! } ?? longest
    }

    /// The window that binds the account now. Pools are independent, so the account is usable while any pool is:
    /// the most used usable pool binds, and when every pool is exhausted, the pool back soonest binds.
    func binding(now: Date) -> Window? {
        let groups = pools.isEmpty ? [windows] : pools.map { windows(in: $0) }
        let worsts = groups.compactMap { Self.worst(Self.current($0, now: now)) }
        return worsts.filter { $0.pace.state != "exhausted" }.max { $0.usedPercent! < $1.usedPercent! }
            ?? worsts.min { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
    }

    /// The window a reader sees, from the binding pool: the longest, unless a shorter one is nearly spent.
    func display(now: Date, alert: Double) -> Window? {
        let pool = binding(now: now)?.pool
        return Self.shown(Self.current(pools.isEmpty ? windows : windows(in: pool), now: now), alert: alert)
    }

    /// True when a shorter window has taken the display from the longest, so the row names it.
    func showsShortWindow(now: Date, alert: Double) -> Bool {
        guard let shown = display(now: now, alert: alert) else { return false }
        let group = Self.current(pools.isEmpty ? windows : windows(in: shown.pool), now: now)
        return group.contains { ($0.windowMinutes ?? 0) > (shown.windowMinutes ?? 0) }
    }

    /// One entry per pool, for its ring: the window shown for that pool, nil once every reading has reset.
    func poolRings(now: Date, alert: Double) -> [(pool: String?, window: Window?)] {
        (pools.isEmpty ? [nil] : pools.map(Optional.some)).map { pool in
            (pool, Self.shown(Self.current(pool == nil ? windows : windows(in: pool), now: now), alert: alert))
        }
    }

    /// Exhausted pools other than the binding one, each with the window that keeps it exhausted.
    func spentPools(now: Date) -> [Window] {
        let bound = binding(now: now)?.pool
        return pools.filter { $0 != bound }.compactMap { Self.worst(Self.current(windows(in: $0), now: now)) }
            .filter { $0.pace.state == "exhausted" }
    }

    /// When the account is usable again: the report's time, or the binding window's reset.
    func back(now: Date) -> Date? {
        backAt ?? (binding(now: now).flatMap { $0.pace.state == "exhausted" ? $0.resetAt : nil })
    }

    /// Drawing on credits, per the report, until an exhausted window behind it resets.
    func isDrawing(now: Date) -> Bool {
        drawingCredits == true && (binding(now: now)?.pace.state == "exhausted" || !windows.contains { $0.pace.state == "exhausted" })
    }

    /// 0 usable, 1 drawing on credits, 2 no reading, 3 exhausted, 4 signed out.
    func rank(_ now: Date) -> Int {
        isSignedOut ? 4 : !hasReading ? 2 : isDrawing(now: now) ? 1 : binding(now: now)?.pace.state == "exhausted" ? 3 : 0
    }

    /// "Claude · Anese". The CLI names accounts; with an older CLI the label stands in, and `default` adds nothing.
    var title: String {
        let person = providerName != nil ? displayName : account == "default" ? nil : account
        return [providerName ?? Fmt.human(provider), person].compactMap { $0 }.joined(separator: " · ")
    }

    var planTitle: String? { planLabel ?? plan.map(Fmt.human) }
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
                burn.map { "about \(money($0 < 10 ? $0 : $0.rounded(), entry.currency))/h" }].compactMap { $0 }.joined(separator: " · ")
    }

    /// "2 banked resets · soonest expires 3 Nov".
    static func resets(_ resets: QuotaReport.Resets) -> String {
        let lead = resets.expiresAt.count > 1 ? "soonest expires" : "expires"
        return "\(resets.available) banked reset\(resets.available == 1 ? "" : "s")"
            + (resets.expiresAt.first.map { " · \(lead) \(day($0))" } ?? "")
    }

    /// "next in 4m", for the footer line.
    static func next(_ schedule: QuotaReport.Schedule, now: Date) -> String {
        until(schedule.nextDueAt, now: now).map { "next in \($0)" } ?? "next due now"
    }

    /// "Next check in 4m · active", for the footer's tooltip.
    static func nextInFull(_ schedule: QuotaReport.Schedule, now: Date) -> String {
        (until(schedule.nextDueAt, now: now).map { "Next check in \($0)" } ?? "Next check due now")
            + (schedule.reason.map { " · " + $0.replacingOccurrences(of: "_", with: " ") } ?? "")
    }

    /// A reset in a row, after its symbol: "4h 40m" within a day, otherwise "Thu 21 Mar".
    static func soon(_ date: Date?, now: Date) -> String? {
        guard let date, date > now else { return nil }
        return date.timeIntervalSince(now) < 86_400 ? until(date, now: now)! : self.date(date)
    }

    /// "Weekly all", never "weekly_all:2": no underscores or colons reach the reader.
    static func human(_ raw: String) -> String {
        let text = raw.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: ":", with: " ")
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    /// The first letter capitalised, the rest left alone.
    static func sentence(_ text: String) -> String { text.prefix(1).uppercased() + text.dropFirst() }

    // Weekday and the rest are formatted apart, so no locale puts a comma between them.
    private static func weekday(_ date: Date) -> String { date.formatted(.dateTime.weekday(.abbreviated)) }

    /// "Tue 19 Mar".
    static func date(_ date: Date) -> String { "\(weekday(date)) \(date.formatted(.dateTime.day().month(.abbreviated)))" }

    /// "Mon 9 am" within a week either way, otherwise "Tue 19 Mar".
    static func moment(_ date: Date, now: Date) -> String {
        guard abs(date.timeIntervalSince(now)) < 6.5 * 86_400 else { return self.date(date) }
        let hour = Date.FormatStyle().hour(.defaultDigits(amPM: .abbreviated))
        return "\(weekday(date)) \(date.formatted(Calendar.current.component(.minute, from: date) == 0 ? hour : hour.minute()))"
    }

    /// The exact instant, for a tooltip: "Tue 19 Mar, 9:00 am".
    static func exact(_ date: Date) -> String { date.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated).hour().minute()) }

    /// A reset is a fact: "resets in 4h 40m" within a day, otherwise "resets Tue 19 Mar".
    static func resets(_ date: Date?, now: Date) -> String? {
        guard let date, date > now else { return nil }
        return date.timeIntervalSince(now) < 86_400 ? "resets in \(until(date, now: now)!)" : "resets \(self.date(date))"
    }

    /// "back in 4h 40m", "back Mon 9 am", or "back since Mon 9 am" once it has passed.
    static func back(_ date: Date?, now: Date) -> String {
        guard let date else { return "reset time unknown" }
        if date <= now { return "back since \(moment(date, now: now))" }
        return date.timeIntervalSince(now) < 86_400 ? "back in \(until(date, now: now)!)" : "back \(moment(date, now: now))"
    }

    /// An estimate, labelled as one, and only when the window would run out before it resets.
    static func projection(_ window: QuotaReport.Window, now: Date) -> String? {
        guard window.pace.state != "exhausted", let out = window.pace.runsOutAt, out > now,
              window.resetAt.map({ out < $0 }) ?? true else { return nil }
        if out.timeIntervalSince(now) < 86_400 { return "At this pace, empty in \(until(out, now: now)!)" }
        let day = abs(out.timeIntervalSince(now)) < 6.5 * 86_400 ? weekday(out) : date(out)
        return "At this pace, empty by \(day)"
    }

    /// "Gemini back Mon 9 am", for an exhausted pool that does not bind the account.
    static func spent(_ window: QuotaReport.Window, now: Date) -> String {
        "\(window.pool.map(human) ?? window.title) \(back(window.resetAt, now: now))"
    }

    /// What a row says beside the account's name. The section header already names the state and the ring shows
    /// the use, so the row adds only what they cannot: the pool and any shorter window taking the display, the
    /// reset after a symbol, and when an exhausted or signed-out account comes back.
    struct Row {
        var lead: String? = nil       // "Claude models", "5-hour", or an unreadable account's error
        var reset: String? = nil      // "Thu 21 Mar", shown after the reset symbol
        var trailing: String? = nil   // "11%", or "back Sun 10 am" for an account that is not usable
        var spoken: [String] = []
    }

    static func row(_ account: QuotaReport.Account, now: Date, alert: Double) -> Row {
        if account.isSignedOut {
            let when = account.backAt.map { back($0, now: now) } ?? account.observedAt.map { "read \(ago($0, now: now))" }
            return Row(trailing: when, spoken: ["signed out", when].compactMap { $0 })
        }
        if !account.hasReading {
            let why = account.error ?? "Not reported"
            return Row(lead: why, spoken: [why])
        }
        let binding = account.binding(now: now)
        if account.isDrawing(now: now) {
            let line = drawing(binding, now: now)
            return Row(lead: line, spoken: [line])
        }
        if let binding, binding.pace.state == "exhausted" {
            let when = back(binding.resetAt, now: now)
            return Row(trailing: when, spoken: ["exhausted", when])
        }
        guard let window = account.display(now: now, alert: alert) else { return Row(lead: "No current reading", spoken: ["no current reading"]) }
        let lead = [window.poolLabel ?? window.pool.map(human), account.showsShortWindow(now: now, alert: alert) ? window.title : nil]
            .compactMap { $0 }.joined(separator: " · ")
        let used = percent(window.usedPercent ?? 0)
        return Row(lead: lead.isEmpty ? nil : lead, reset: soon(window.resetAt, now: now), trailing: used,
                   spoken: [lead.isEmpty ? nil : lead, "\(used) used", resets(window.resetAt, now: now)].compactMap { $0 })
    }

    /// "Allowance back in 3d 2h" for an account drawing on credits; its section already says so.
    static func drawing(_ window: QuotaReport.Window?, now: Date) -> String {
        let back = window?.pace.state == "exhausted" ? window?.resetAt.map { self.back($0, now: now) } : nil
        return back.map { "Allowance " + $0 } ?? "Allowance spent"
    }
}
