import Foundation
import Observation
import ProvenantDesign
import ProvenantKit

struct TrendPoint: Equatable, Sendable, Identifiable {
    let date: Date
    let percent: Double
    var id: Date { date }
}

struct QuotaRow: Identifiable, Equatable, Sendable {
    struct Window: Equatable, Sendable, QuotaWindow {
        /// The wire pool key, kept for accessibility and reset reporting.
        let name: String
        /// The adapter's pool group, nil for a window that stands alone.
        let pool: String?
        /// One human label, e.g. "Gemini weekly", not the raw key.
        let label: String
        let percent: Double
        let resetsAt: Date?
        let lowerBound: Bool
        let windowMinutes: Int?
        let advisory: Bool?

        var resetAt: Date? { resetsAt }
        var usedPercent: Double? { percent }
        var paceState: String { percent >= 100 ? "exhausted" : "on_pace" }
        /// The menu bar floors a used percentage; use the same rule so both surfaces read the same number.
        var displayPercent: Int { Int(max(percent, 0).rounded(.down)) }
        var value: String { "\(lowerBound ? "≥" : "")\(displayPercent)%" }
    }
    struct Refusal: Equatable, Sendable {
        let model: String
        let pool: String?
        let resetsAt: Date?
        var id: String { "\(pool ?? "")/\(model)" }
    }
    let id: String
    let title: String
    let provider: String
    let status: String
    let refusals: [Refusal]
    let windows: [Window]
    /// The same headline window the menu bar shows for this account.
    let headlineWindow: Window?
    let checkedAgo: TimeInterval?
    let error: String?
    let trend: [TrendPoint]
}

struct QuotaSnapshot: Equatable, Sendable {
    let rows: [QuotaRow]
    let generatedAt: Date
    let historyAvailable: Bool
}

/// The Quota screen: `quota show` with a trend from `quota history`. The trend is optional, so a history failure
/// leaves the table.
@MainActor @Observable
final class QuotaPanelModel {
    let reader = Reader<QuotaSnapshot>()
    private let client: ConsoleClient
    private let now: () -> Date

    init(client: ConsoleClient, now: @escaping () -> Date = { .now }) { self.client = client; self.now = now }

    func refresh() async {
        await reader.load(now: now) { [client] in
            async let show = client.quota()
            async let history = try? await client.quotaHistory()
            return Self.snapshot(try await show, history: await history)
        }
    }

    nonisolated static func snapshot(_ show: QuotaShow, history: QuotaHistory?) -> QuotaSnapshot {
        let generatedAt = Date(timeIntervalSince1970: TimeInterval(show.generatedUnixSeconds))
        let rows = show.accounts.map { account -> QuotaRow in
            let adapter = account.account.adapter
            let lowerBound = account.snapshot?.details?.coverage == "partial"
            let windows = (account.snapshot?.windows ?? []).map { item -> QuotaRow.Window in
                let presentation = QuotaPresentation.presentation(adapter: adapter, name: item.pool, seconds: item.windowSeconds)
                return QuotaRow.Window(name: item.pool, pool: presentation.pool,
                                       label: QuotaPresentation.windowName(presentation.title, poolLabel: presentation.poolLabel),
                                       percent: Double(item.usedBasisPoints) / 100,
                                       resetsAt: item.resetsUnixSeconds.map { Date(timeIntervalSince1970: TimeInterval($0)) },
                                       lowerBound: lowerBound,
                                       windowMinutes: QuotaPresentation.windowMinutes(adapter: adapter, name: item.pool,
                                                                                     seconds: item.windowSeconds),
                                       advisory: presentation.advisory)
            }
            let choice = QuotaPresentation.Headline(wire: account.display?.headline ?? show.display?.headline)
            let trend = (history?.readings ?? [])
                .filter { $0.provider == adapter && $0.accountKey == account.account.accountKey }
                .compactMap { reading -> TrendPoint? in
                    // A reading is a snapshot at its observation time, not at the report's generation time.
                    // Judging its windows against `generatedAt` would discard every older reading as reset.
                    let observed = Date(timeIntervalSince1970: TimeInterval(reading.observedUnixSeconds))
                    let readingWindows = reading.windows.map { item -> QuotaRow.Window in
                        let presentation = QuotaPresentation.presentation(adapter: adapter, name: item.pool, seconds: item.windowSeconds)
                        return QuotaRow.Window(name: item.pool, pool: presentation.pool, label: presentation.title,
                                               percent: Double(item.usedBasisPoints) / 100,
                                               resetsAt: item.resetsUnixSeconds.map { Date(timeIntervalSince1970: TimeInterval($0)) },
                                               lowerBound: false,
                                               windowMinutes: QuotaPresentation.windowMinutes(adapter: adapter, name: item.pool,
                                                                                             seconds: item.windowSeconds),
                                               advisory: presentation.advisory)
                    }
                    guard let top = QuotaPresentation.headline(readingWindows, now: observed,
                                                               alert: QuotaPresentation.alertPercent, choice: choice) else { return nil }
                    return TrendPoint(date: observed, percent: top.percent)
                }
                .sorted { $0.date < $1.date }
            return QuotaRow(id: account.id, title: account.account.label, provider: adapter, status: account.status,
                            refusals: (account.refusals ?? []).map {
                                QuotaRow.Refusal(model: $0.model, pool: $0.pool, resetsAt: Stamp.parse($0.resetsAt))
                            },
                            windows: windows,
                            headlineWindow: QuotaPresentation.headline(windows, now: generatedAt,
                                                                       alert: QuotaPresentation.alertPercent, choice: choice),
                            checkedAgo: account.ageSeconds.map(TimeInterval.init), error: account.error, trend: trend)
        }
        return QuotaSnapshot(rows: rows, generatedAt: generatedAt, historyAvailable: history != nil)
    }
}
