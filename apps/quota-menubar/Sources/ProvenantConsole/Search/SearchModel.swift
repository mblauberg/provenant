import Foundation
import Observation
import ProvenantKit

/// Content search over attempts' prompts or results: `console search`. Runs when asked, never as the person types.
@MainActor @Observable
final class SearchModel {
    enum Part: String, Sendable { case prompt, result }

    let reader = Reader<SearchResults>()
    private(set) var query = ""
    private(set) var part: Part = .result
    private let client: ConsoleClient

    init(client: ConsoleClient) { self.client = client }

    func run(_ query: String, part: Part) async {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if trimmed != self.query || part != self.part { reader.reset() }
        self.query = trimmed
        self.part = part
        await reader.load { [client] in try await client.search(trimmed, part: part.rawValue) }
    }

    func rerun() async { if !query.isEmpty { await run(query, part: part) } }
}
