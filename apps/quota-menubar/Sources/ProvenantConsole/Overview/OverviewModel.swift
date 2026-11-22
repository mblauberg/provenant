import Foundation
import Observation
import ProvenantKit

/// One thing waiting on the person, with the kind and blocking flag the inbox knows and the overview does not.
struct NeedsYouRow: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let project: String?
    let kind: RequestKind?
    let blocking: Bool
}

struct OverviewSnapshot: Equatable, Sendable {
    let overview: Overview
    let needsYou: [NeedsYouRow]
    /// False when the inbox could not be read, so kinds and blocking flags are unknown rather than absent.
    let inboxJoined: Bool

    /// Everything the overview could not see, at the level each statement deserves.
    var notices: [CoverageNotice] {
        overview.coverage.notices + (inboxJoined ? [] : [.warning("Request kinds are unavailable: the inbox could not be read.")])
    }
}

/// The overview: `console overview` joined with the open inbox. The join is best effort, so an inbox failure
/// leaves the overview readable.
@MainActor @Observable
final class OverviewModel {
    let reader = Reader<OverviewSnapshot>()
    private let console: ConsoleClient, inbox: InboxClient
    private let now: () -> Date

    init(console: ConsoleClient, inbox: InboxClient, now: @escaping () -> Date = { .now }) {
        self.console = console; self.inbox = inbox; self.now = now
    }

    func refresh() async {
        await reader.load(now: now) { [console, inbox] in
            async let overview = console.overview()
            async let requests = try? await inbox.list()
            return Self.join(try await overview, requests: await requests?.items)
        }
    }

    nonisolated static func join(_ overview: Overview, requests: [HumanRequest]?) -> OverviewSnapshot {
        let byID = Dictionary((requests ?? []).map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let rows = overview.needsYou.map { entry -> NeedsYouRow in
            let request = byID[entry.id]
            return NeedsYouRow(id: entry.id, title: entry.title, project: entry.projectLabel ?? request?.projectName,
                               kind: request?.kind, blocking: request?.request.blocking ?? false)
        }
        // Blocking requests first; the CLI's order otherwise.
        let ordered = rows.enumerated().sorted { ($0.element.blocking ? 0 : 1, $0.offset) < ($1.element.blocking ? 0 : 1, $1.offset) }.map(\.element)
        return OverviewSnapshot(overview: overview, needsYou: ordered, inboxJoined: requests != nil)
    }
}
