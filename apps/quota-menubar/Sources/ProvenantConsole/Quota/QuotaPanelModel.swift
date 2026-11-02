import Foundation
import Observation
import ProvenantKit

struct TrendPoint: Equatable, Sendable, Identifiable {
    let date: Date
    let percent: Double
    var id: Date { date }
}

struct QuotaRow: Identifiable, Equatable, Sendable {
    struct Window: Equatable, Sendable {
        let pool: String
        let percent: Double
        let resetsAt: Date?
    }
    let id: String
    let title: String
    let provider: String
    let status: String
    let windows: [Window]
    let checkedAgo: TimeInterval?
    let error: String?
    let trend: [TrendPoint]

    /// The window closest to its limit, the one that binds the account.
    var binding: Window? { windows.max { $0.percent < $1.percent } }
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

    init(client: ConsoleClient) { self.client = client }

    func refresh() async {
        await reader.load { [client] in
            async let show = client.quota()
            async let history = try? await client.quotaHistory()
            return Self.snapshot(try await show, history: await history)
        }
    }

    nonisolated static func snapshot(_ show: QuotaShow, history: QuotaHistory?) -> QuotaSnapshot {
        let rows = show.accounts.map { account -> QuotaRow in
            let windows = (account.snapshot?.windows ?? []).map {
                QuotaRow.Window(pool: $0.pool, percent: Double($0.usedBasisPoints) / 100,
                                resetsAt: $0.resetsUnixSeconds.map { Date(timeIntervalSince1970: TimeInterval($0)) })
            }
            let trend = (history?.readings ?? [])
                .filter { $0.provider == account.account.adapter && $0.accountKey == account.account.accountKey }
                .compactMap { reading -> TrendPoint? in
                    guard let top = reading.windows.map(\.usedBasisPoints).max() else { return nil }
                    return TrendPoint(date: Date(timeIntervalSince1970: TimeInterval(reading.observedUnixSeconds)), percent: Double(top) / 100)
                }
                .sorted { $0.date < $1.date }
            return QuotaRow(id: account.id, title: account.account.label, provider: account.account.adapter, status: account.status,
                            windows: windows, checkedAgo: account.ageSeconds.map(TimeInterval.init), error: account.error, trend: trend)
        }
        return QuotaSnapshot(rows: rows, generatedAt: Date(timeIntervalSince1970: TimeInterval(show.generatedUnixSeconds)),
                             historyAvailable: history != nil)
    }
}
