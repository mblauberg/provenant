import Foundation

/// The fields the menu and the console need to choose and name a quota window. Both surfaces project the same
/// `quota show` wire shape into their own row types; conforming those rows lets one rule drive both.
public protocol QuotaWindow: Sendable {
    var name: String { get }
    var pool: String? { get }
    var usedPercent: Double? { get }
    var windowMinutes: Int? { get }
    var resetAt: Date? { get }
    var advisory: Bool? { get }
    var paceState: String { get }
}

public extension QuotaWindow {
    func isExpired(_ now: Date) -> Bool { resetAt.map { $0 <= now } ?? false }
    var setsState: Bool { advisory != true && resetAt != nil }
    var isSpent: Bool { paceState == "exhausted" }
}

/// Naming and headline selection shared by the menu bar and the console's quota panel.
public enum QuotaPresentation {
    /// A short window at or above this level leads an automatic headline.
    public static let alertPercent: Double = 90

    /// Which window leads an account's row. A missing or unknown choice is automatic.
    public enum Headline: String, Equatable, Sendable {
        case auto, fiveHour = "5h", weekly

        public init(wire: String?) { self = wire.flatMap(Headline.init(rawValue:)) ?? .auto }
    }

    /// A human name for a window and, for pooled adapters, its pool. `gemini.weekly:2` keeps its ordinal so two
    /// weekly windows of one pool do not read as the same row.
    public static func presentation(adapter: String, name: String, seconds: UInt64?)
        -> (title: String, pool: String?, poolLabel: String?, advisory: Bool) {
        let duration = durationTitle(seconds)
        if adapter == "claude", name.hasPrefix("weekly_scoped:") {
            let model = String(name.dropFirst("weekly_scoped:".count))
            return ("Weekly · \(Fmt.human(model))", nil, nil, true)
        }
        if adapter == "agy" {
            for (prefix, title) in [("gemini.", "Gemini"), ("claude.", "Claude")] where name.hasPrefix(prefix) {
                let kind = String(name.dropFirst(prefix.count))
                let suffix = ordinalSuffix(kind)
                return ((duration ?? namedWindowTitle(kind)) + suffix, String(prefix.dropLast()), title, false)
            }
        }
        if adapter == "codex", let separator = name.lastIndex(of: ".") {
            let pool = String(name[..<separator])
            let kind = String(name[name.index(after: separator)...])
            if ["primary", "secondary"].contains(kind) {
                return (duration ?? namedWindowTitle(kind), pool, modelPoolTitle(pool), false)
            }
        }
        return ((duration ?? namedWindowTitle(name)) + ordinalSuffix(name), nil, nil, false)
    }

    /// "Gemini weekly" or "Weekly": the single short label shown for a window.
    public static func windowName(_ title: String, poolLabel: String?) -> String {
        guard let poolLabel else { return title }
        return "\(poolLabel) \(String(title.prefix(1)).lowercased() + title.dropFirst())"
    }

    /// The window length in minutes, falling back to the adapter's usual length for an unrecorded window.
    public static func windowMinutes(adapter: String, name: String, seconds: UInt64?) -> Int? {
        seconds.map { Int(clamping: $0 / 60) } ?? fallbackWindowMinutes(adapter: adapter, name: name)
    }

    /// Windows that can currently determine state: a reading exists, it has not reset, and it is not advisory.
    public static func current<W: QuotaWindow>(_ windows: [W], now: Date) -> [W] {
        windows.filter { $0.usedPercent != nil && !$0.isExpired(now) && $0.setsState }
    }

    /// The exhausted window nearest its reset, else the most used one.
    public static func worst<W: QuotaWindow>(_ windows: [W]) -> W? {
        windows.filter(\.isSpent).max { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) }
            ?? windows.max { ($0.usedPercent ?? -1) < ($1.usedPercent ?? -1) }
    }

    /// The window that binds the account: the highest reading outside exhausted pools, or the exhausted one
    /// resetting soonest when every pool is spent.
    public static func binding<W: QuotaWindow>(in windows: [W], now: Date) -> W? {
        let worsts = groups(in: windows).compactMap { worst(current($0.windows, now: now)) }
        let available = worsts.filter { !$0.isSpent }
        if let binding = available.max(by: { ($0.usedPercent ?? 0) < ($1.usedPercent ?? 0) }) { return binding }
        return worsts.min(by: { ($0.resetAt ?? .distantFuture) < ($1.resetAt ?? .distantFuture) })
    }

    /// The window an automatic headline shows: the earliest-resetting exhausted window, otherwise a short window
    /// at the alert level, otherwise the longest window in the binding pool.
    public static func shown<W: QuotaWindow>(_ windows: [W], alert: Double) -> W? {
        if let spent = worst(windows), spent.isSpent { return spent }
        let withUsage = windows.filter { $0.usedPercent != nil }
        let longest = withUsage.max {
            ($0.windowMinutes ?? 0, $0.usedPercent ?? 0) < ($1.windowMinutes ?? 0, $1.usedPercent ?? 0)
        }
        let alerts = withUsage.filter {
            ($0.usedPercent ?? 0) >= alert && ($0.windowMinutes ?? 0) < (longest?.windowMinutes ?? 0)
        }
        return alerts.max { ($0.usedPercent ?? 0) < ($1.usedPercent ?? 0) } ?? longest
    }

    /// The account's headline window under its chosen (or automatic) headline rule.
    public static func headline<W: QuotaWindow>(_ windows: [W], now: Date, alert: Double, choice: Headline) -> W? {
        let pool = binding(in: windows, now: now)?.pool
        let group = windows.contains(where: { $0.pool != nil })
            ? windows.filter { $0.pool == nil || $0.pool == pool } : windows
        switch choice {
        case .auto: return shown(group, alert: alert)
        case .fiveHour: return group.min { ($0.windowMinutes ?? .max) < ($1.windowMinutes ?? .max) } ?? shown(group, alert: alert)
        case .weekly: return group.max { ($0.windowMinutes ?? 0) < ($1.windowMinutes ?? 0) } ?? shown(group, alert: alert)
        }
    }

    /// All windows grouped by pool, with the unpooled windows included in every group.
    public static func groups<W: QuotaWindow>(in windows: [W]) -> [(pool: String?, windows: [W])] {
        var result: [(pool: String?, windows: [W])] = []
        let global = windows.filter { $0.pool == nil }
        if !global.isEmpty { result.append((nil, global)) }
        var pools: [String] = []
        for window in windows where window.pool != nil && !pools.contains(window.pool!) { pools.append(window.pool!) }
        result += pools.map { pool in (Optional(pool), windows.filter { $0.pool == nil || $0.pool == pool }) }
        return result.isEmpty ? [(nil, windows)] : result
    }

    private static func ordinalSuffix(_ name: String) -> String {
        guard let colon = name.lastIndex(of: ":"), let ordinal = Int(name[name.index(after: colon)...]), ordinal > 1 else { return "" }
        return " (\(ordinal))"
    }

    private static func modelPoolTitle(_ slug: String) -> String {
        slug.split(separator: "-").map { part in
            let word = String(part)
            return word.lowercased() == "gpt" ? "GPT" : Fmt.human(word)
        }.joined(separator: " ")
    }

    private static func durationTitle(_ seconds: UInt64?) -> String? {
        guard let seconds else { return nil }
        let hour: UInt64 = 3_600, day: UInt64 = 86_400
        if (4 * hour...6 * hour).contains(seconds) { return "5-hour" }
        if (22 * hour...26 * hour).contains(seconds) { return "Daily" }
        if (6 * day...8 * day).contains(seconds) { return "Weekly" }
        if (27 * day...33 * day).contains(seconds) { return "Monthly" }
        if seconds >= hour && seconds.isMultiple(of: hour) { return "\(seconds / hour)-hour" }
        if seconds >= 60 && seconds.isMultiple(of: 60) { return "\(seconds / 60)-minute" }
        return "\(seconds)-second"
    }

    private static func namedWindowTitle(_ name: String) -> String {
        switch name.lowercased() {
        case "five_hour", "5h": "5-hour"
        case "seven_day", "weekly", "weekly_all": "Weekly"
        case "thirty_day": "Monthly"
        case "billing_cycle": "Billing cycle"
        case "session": "Session"
        default: Fmt.human(name)
        }
    }

    private static func fallbackWindowMinutes(adapter: String, name: String) -> Int? {
        switch name.lowercased() {
        case "five_hour", "5h", "gemini.5h", "session": return 300
        case "seven_day", "weekly", "weekly_all", "gemini.weekly", "claude.weekly": return 10_080
        case "thirty_day": return 43_200
        default:
            if adapter == "claude", name.hasPrefix("weekly_scoped:") { return 10_080 }
            return nil
        }
    }
}

/// `Fmt.human` is owned by the menu module's presentation layer; this is the small shared equivalent for wire keys.
private enum Fmt {
    static func human(_ raw: String) -> String {
        let text = raw.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: ":", with: " ")
        return String(text.prefix(1)).uppercased() + text.dropFirst()
    }
}
