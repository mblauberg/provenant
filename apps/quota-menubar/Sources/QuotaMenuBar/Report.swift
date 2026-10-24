import Foundation

/// A projection of the quota crate's versioned report for the menu-bar views.
struct QuotaReport {
    static let schemaVersion: UInt64 = 1
    let accounts: [Account]
    let generatedAt: Date
    let nextDueAt: Date?
    let refreshing: [String]
    let reportPath: String
    let coverage: Coverage
    let sources: [Source]
    var alertPercent: Double { 90 }

    struct Coverage { let total, known: UInt64 }
    struct Source {
        let adapter, status: String
        let error: String?
        let nextDueAt: Date?
        var title: String { QuotaReport.providerName(adapter) }
        var statusTitle: String { QuotaReport.statusTitle(status) }
    }

    struct Account: Identifiable {
        let provider, account, accountKey: String
        let plan, error: String?
        let active: Bool?
        let priority: Double?
        let status: String
        let providerName, displayName, planLabel: String?
        let backAt, observedAt: Date?
        let windows: [Window]
        let unreported: [String]?
        let drawingCredits: Bool?
        let creditBurnPerHour: Double?
        let schedule: Schedule?
        private let hasSnapshot: Bool

        var id: String { "\(provider)/\(accountKey)" }
        var hasReading: Bool { hasSnapshot && ["available", "credit_only", "exhausted", "stale"].contains(status) }
        var requiresAuth: Bool { status == "auth_required" }
        var next: Schedule? { schedule }
        var credits: [Credit] { [] }
        var banked: Resets? { nil }
        var statusTitle: String {
            QuotaReport.statusTitle(status)
        }

        fileprivate init(_ row: QuotaWireAccount, generatedAt: Date) throws {
            let rowStatus = row.status
            provider = row.account.adapter
            account = row.account.label
            accountKey = row.account.accountKey
            guard QuotaReport.validStatuses.contains(rowStatus) else {
                throw QuotaReport.invalid("unknown account status")
            }
            status = rowStatus
            error = row.error
            hasSnapshot = row.snapshot != nil
            providerName = QuotaReport.providerName(provider)
            displayName = account == "default" || account.isEmpty ? nil : account
            plan = nil
            planLabel = nil
            active = nil
            priority = nil
            unreported = nil
            drawingCredits = rowStatus == "credit_only"
            creditBurnPerHour = nil
            observedAt = row.ageSeconds.map { Date(timeIntervalSince1970: generatedAt.timeIntervalSince1970 - Double($0)) }
            let mappedWindows = (row.snapshot?.windows ?? []).map { item in
                let paceState = ["exhausted", "credit_only"].contains(rowStatus) ? "exhausted" : "on_pace"
                return Window(name: item.pool, label: Fmt.human(item.pool), usedPercent: Double(item.usedBasisPoints) / 100,
                              windowMinutes: nil, resetAt: item.resetsUnixSeconds.map(QuotaReport.date), usedAmount: nil,
                              limitAmount: nil, currency: nil, advisory: false, pool: item.pool, poolLabel: nil,
                              pace: Pace(state: paceState, runsOutAt: nil, summary: nil))
            }
            windows = mappedWindows
            backAt = rowStatus == "exhausted" ? mappedWindows.compactMap(\.resetAt).min() : nil
            schedule = row.nextDueUnixSeconds.map { Schedule(nextDueAt: QuotaReport.date($0), reason: nil) }
        }
    }

    struct Schedule { let nextDueAt: Date; let reason: String? }
    struct Credit {
        let balance: Double?
        let unlimited: Bool
        let currency, unit: String?
        let expiresAt: Date?
    }
    struct Resets { let available: Int; let expiresAt: [Date] }
    struct Window: Identifiable {
        var id: String { name }
        let name: String
        let label: String?
        let usedPercent: Double?
        let windowMinutes: Int?
        let resetAt: Date?
        let usedAmount, limitAmount: Double?
        let currency: String?
        let advisory: Bool?
        let pool, poolLabel: String?
        let pace: Pace
        var title: String { label ?? Fmt.human(name) }
        var fullTitle: String { poolLabel.map { "\($0) · \(title)" } ?? title }
        var setsState: Bool { advisory != true && resetAt != nil }
        func isExpired(_ now: Date) -> Bool { resetAt.map { $0 <= now } ?? false }
    }
    struct Pace { let state: String; let runsOutAt: Date?; let summary: String? }

    private static let validStatuses: Set<String> = ["available", "credit_only", "exhausted", "stale", "unknown", "auth_required", "unsupported"]
    private static func date(_ seconds: Int64) -> Date { Date(timeIntervalSince1970: Double(seconds)) }
    private static func providerName(_ adapter: String) -> String {
        switch adapter {
        case "agy": "Antigravity"
        case "claude": "Claude"
        case "codex": "Codex"
        case "cursor": "Cursor"
        case "opencode": "OpenCode"
        default: Fmt.human(adapter)
        }
    }
    private static func statusTitle(_ status: String) -> String {
        switch status {
        case "stale": "Stale"
        case "unknown": "Unknown"
        case "auth_required": "Authentication required"
        case "unsupported": "Unsupported"
        case "credit_only": "Exhausted, drawing on credits"
        default: Fmt.human(status)
        }
    }
    private static func invalid(_ message: String) -> DecodingError {
        .dataCorrupted(.init(codingPath: [], debugDescription: message))
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    static func decode(_ data: Data) throws -> QuotaReport {
        try project(decoder().decode(QuotaWireReport.self, from: data))
    }

    static func decodeEnvelope(_ data: Data, command: String) throws -> QuotaReport {
        let envelope = try decoder().decode(QuotaEnvelope<QuotaWireReport>.self, from: data)
        guard envelope.schemaVersion == schemaVersion, envelope.command == command, envelope.status == "ok" else {
            throw invalid("unexpected quota command envelope")
        }
        guard let report = envelope.data else { throw invalid(envelope.error ?? "quota command returned no report") }
        return try project(report)
    }

    private static func project(_ wire: QuotaWireReport) throws -> QuotaReport {
        guard wire.schemaVersion == schemaVersion else { throw invalid("unsupported report version") }
        guard !wire.reportPath.isEmpty, wire.reportPath.hasPrefix("/") else { throw invalid("report path is not absolute") }
        let generatedAt = date(wire.generatedUnixSeconds)
        let accounts = try wire.accounts.map { try Account($0, generatedAt: generatedAt) }
        let sources = try (wire.sources ?? []).map { source -> Source in
            guard validStatuses.contains(source.status) else { throw invalid("unknown source status") }
            return Source(adapter: source.adapter, status: source.status, error: source.error,
                          nextDueAt: source.nextDueUnixSeconds.map(date))
        }
        return QuotaReport(accounts: accounts, generatedAt: generatedAt,
                           nextDueAt: wire.nextDueUnixSeconds.map(date), refreshing: wire.refreshingIds,
                           reportPath: wire.reportPath, coverage: Coverage(total: wire.coverage.total, known: wire.coverage.known),
                           sources: sources)
    }

    func isRefreshing(now: Date) -> Bool { !refreshing.isEmpty && now.timeIntervalSince(generatedAt) < 60 }
    var nextCheck: Schedule? { nextDueAt.map { Schedule(nextDueAt: $0, reason: nil) } }
    func sorted(now: Date) -> [Account] {
        accounts.enumerated().sorted { ($0.element.rank(now), $0.element.priority ?? 100, $0.offset)
            < ($1.element.rank(now), $1.element.priority ?? 100, $1.offset) }.map(\.element)
    }
    func top(now: Date) -> (account: Account, window: Window)? {
        accounts.filter { $0.rank(now) == 0 }.compactMap { account in account.binding(now: now).map { (account, $0) } }
            .max { $0.window.usedPercent! < $1.window.usedPercent! }
    }
    func featured(now: Date) -> (account: Account, window: Window)? {
        top(now: now).map { ($0.account, $0.account.display(now: now, alert: alertPercent) ?? $0.window) }
    }
    func exhausted(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 3 } }
    var iconOnly: Bool { accounts.isEmpty }
    func drawing(now: Date) -> [Account] { accounts.filter { $0.rank(now) == 1 } }
    func headline(now: Date) -> Double? {
        top(now: now)?.window.usedPercent ?? (exhausted(now: now).isEmpty && drawing(now: now).isEmpty ? nil : 100)
    }
}

private struct QuotaWireReport: Decodable {
    let schemaVersion: UInt64
    let generatedUnixSeconds: Int64
    let accounts: [QuotaWireAccount]
    let coverage: QuotaWireCoverage
    let refreshingIds: [String]
    let nextDueUnixSeconds: Int64?
    let reportPath: String
    let sources: [QuotaWireSource]?
}
private struct QuotaEnvelope<Payload: Decodable>: Decodable {
    let schemaVersion: UInt64
    let command, status: String
    let data: Payload?
    let error: String?
}
private struct QuotaWireCoverage: Decodable { let total, known: UInt64 }
private struct QuotaWireAccount: Decodable {
    let account: QuotaWireScope
    let snapshot: QuotaWireSnapshot?
    let status: String
    let ageSeconds: UInt64?
    let nextDueUnixSeconds: Int64?
    let refreshing: Bool
    let error: String?
}
private struct QuotaWireScope: Decodable { let adapter, accountKey, label: String }
private struct QuotaWireSnapshot: Decodable { let windows: [QuotaWireWindow] }
private struct QuotaWireWindow: Decodable { let pool: String; let usedBasisPoints: UInt64; let resetsUnixSeconds: Int64? }
private struct QuotaWireSource: Decodable { let adapter, status: String; let error: String?; let nextDueUnixSeconds: Int64? }

extension QuotaReport.Account {
    typealias Window = QuotaReport.Window
    var pools: [String] { windows.compactMap(\.pool).reduce(into: []) { if !$0.contains($1) { $0.append($1) } } }
    func windows(in pool: String?) -> [Window] { windows.filter { $0.pool == nil || $0.pool == pool } }
    static func current(_ windows: [Window], now: Date) -> [Window] {
        windows.filter { $0.usedPercent != nil && !$0.isExpired(now) && $0.setsState }
    }
    static func worst(_ windows: [Window]) -> Window? {
        windows.filter { $0.pace.state == "exhausted" }.max { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
            ?? windows.max { $0.usedPercent! < $1.usedPercent! }
    }
    static func shown(_ windows: [Window], alert: Double) -> Window? {
        if let spent = worst(windows), spent.pace.state == "exhausted" { return spent }
        let longest = windows.max { ($0.windowMinutes ?? 0, $0.usedPercent!) < ($1.windowMinutes ?? 0, $1.usedPercent!) }
        let alerts = windows.filter { $0.usedPercent! >= alert && ($0.windowMinutes ?? 0) < (longest?.windowMinutes ?? 0) }
        return alerts.max { $0.usedPercent! < $1.usedPercent! } ?? longest
    }
    func binding(now: Date) -> Window? {
        let groups = pools.isEmpty ? [windows] : pools.map { windows(in: $0) }
        let worsts = groups.compactMap { Self.worst(Self.current($0, now: now)) }
        return worsts.filter { $0.pace.state != "exhausted" }.max { $0.usedPercent! < $1.usedPercent! }
            ?? worsts.min { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
    }
    func display(now: Date, alert: Double) -> Window? {
        let pool = binding(now: now)?.pool
        return Self.shown(Self.current(pools.isEmpty ? windows : windows(in: pool), now: now), alert: alert)
    }
    func showsShortWindow(now: Date, alert: Double) -> Bool {
        guard let shown = display(now: now, alert: alert) else { return false }
        let group = Self.current(pools.isEmpty ? windows : windows(in: shown.pool), now: now)
        return group.contains { ($0.windowMinutes ?? 0) > (shown.windowMinutes ?? 0) }
    }
    func poolRings(now: Date, alert: Double) -> [(pool: String?, window: Window?)] {
        (pools.isEmpty ? [nil] : pools.map(Optional.some)).map { pool in
            (pool, Self.shown(Self.current(pool == nil ? windows : windows(in: pool), now: now), alert: alert))
        }
    }
    func spentPools(now: Date) -> [Window] {
        let bound = binding(now: now)?.pool
        return pools.filter { $0 != bound }.compactMap { Self.worst(Self.current(windows(in: $0), now: now)) }
            .filter { $0.pace.state == "exhausted" }
    }
    func back(now: Date) -> Date? {
        backAt ?? (binding(now: now).flatMap { $0.pace.state == "exhausted" ? $0.resetAt : nil })
    }
    func isDrawing(now _: Date) -> Bool { drawingCredits == true }
    func rank(_ now: Date) -> Int {
        switch status {
        case "auth_required": 4
        case "exhausted": 3
        case "credit_only": 1
        case "available": 0
        default: 2
        }
    }
    var title: String { [providerName ?? Fmt.human(provider), displayName].compactMap { $0 }.joined(separator: " · ") }
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

    /// The allowance estimate and reserve runway, only when allowance would be spent before reset.
    static func projection(_ window: QuotaReport.Window, now: Date) -> String? {
        guard window.pace.state != "exhausted", let out = window.pace.runsOutAt, out > now,
              window.resetAt.map({ out < $0 }) ?? true else { return nil }
        if window.pace.state == "ahead_of_pace", let summary = window.pace.summary { return sentence(summary) }
        if out.timeIntervalSince(now) < 86_400 { return "At this pace, allowance lasts about \(until(out, now: now)!)" }
        let day = abs(out.timeIntervalSince(now)) < 6.5 * 86_400 ? weekday(out) : date(out)
        return "At this pace, allowance lasts until \(day)"
    }

    /// "Gemini back Mon 9 am", for an exhausted pool that does not bind the account.
    static func spent(_ window: QuotaReport.Window, now: Date) -> String {
        "\(window.pool.map(human) ?? window.title) \(back(window.resetAt, now: now))"
    }

    /// What a row says beside the account's name. The section header already names the state and the ring shows
    /// the use, so the row adds only what they cannot: the pool and any shorter window taking the display, the
    /// reset after a symbol, and when an exhausted account comes back.
    struct Row {
        var lead: String? = nil       // "Claude models", "5-hour", or an unreadable account's error
        var reset: String? = nil      // "Thu 21 Mar", shown after the reset symbol
        var trailing: String? = nil   // "11%", or "back Sun 10 am" for an account that is not usable
        var spoken: [String] = []
    }

    static func row(_ account: QuotaReport.Account, now: Date, alert: Double) -> Row {
        if account.requiresAuth {
            let when = account.backAt.map { back($0, now: now) } ?? account.observedAt.map { "read \(ago($0, now: now))" }
            return Row(lead: account.statusTitle, trailing: when, spoken: [account.statusTitle, when].compactMap { $0 })
        }
        if !account.hasReading {
            let why = account.error ?? account.statusTitle
            return Row(lead: why, spoken: [why])
        }
        let binding = account.binding(now: now)
        if account.isDrawing(now: now) {
            let line = drawing(binding, now: now)
            return Row(lead: account.statusTitle + " · " + line, spoken: ["exhausted, drawing on credits", line])
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
