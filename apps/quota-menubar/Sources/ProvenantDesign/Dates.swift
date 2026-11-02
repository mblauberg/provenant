import Foundation

/// Timestamps the CLI prints (RFC 3339, with or without fractional seconds) as people read them.
public enum Stamp {
    public static func parse(_ text: String?) -> Date? {
        guard let text else { return nil }
        if let date = try? Date(text, strategy: .iso8601) { return date }
        return try? Date(text, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }

    /// "2 minutes ago", "yesterday", "in 3 hours". An unreadable stamp comes back as it was, so nothing is hidden.
    public static func relative(_ text: String?, now: Date, style: RelativeDateTimeFormatter.UnitsStyle = .full) -> String {
        guard let text else { return "" }
        guard let date = parse(text) else { return text }
        return relative(date, now: now, style: style)
    }

    public static func relative(_ date: Date, now: Date, style: RelativeDateTimeFormatter.UnitsStyle = .full) -> String {
        if abs(now.timeIntervalSince(date)) < 45 { return "just now" }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = style
        formatter.dateTimeStyle = .named
        return formatter.localizedString(for: date, relativeTo: now)
    }

    /// "2 Nov 2026 at 09:14", for a tooltip or a detail line.
    public static func absolute(_ text: String?) -> String {
        guard let text else { return "" }
        guard let date = parse(text) else { return text }
        return date.formatted(date: .abbreviated, time: .shortened)
    }

    /// A duration in milliseconds as "840 ms", "3.2 s" or "2 min 5 s".
    public static func duration(milliseconds: UInt64) -> String {
        switch milliseconds {
        case ..<1_000: return "\(milliseconds) ms"
        case ..<60_000: return String(format: "%.1f s", Double(milliseconds) / 1_000)
        default: return "\(milliseconds / 60_000) min \(milliseconds % 60_000 / 1_000) s"
        }
    }
}

public enum Counts {
    /// 1,234 as "1.2K", 3,400,000 as "3.4M"; small numbers are written out.
    public static func compact(_ value: UInt64) -> String {
        value.formatted(.number.notation(.compactName))
    }
}
