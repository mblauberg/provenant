import Foundation
import Observation
import ProvenantKit

/// The inspector's subject: one attempt's detail and the text of its result or prompt.
@MainActor @Observable
final class AttemptModel {
    enum Part: String, CaseIterable, Identifiable, Sendable {
        case result, prompt
        var id: String { rawValue }
        var title: String { rawValue.capitalized }
    }

    let detail = Reader<AttemptDetail>()
    let output = Reader<AttemptOutput>()
    private(set) var attemptID: String?
    var part: Part = .result
    private let client: ConsoleClient

    init(client: ConsoleClient) { self.client = client }

    /// Points the inspector at an attempt and reads it. The same attempt again only reloads.
    func show(_ id: String) async {
        if attemptID != id {
            attemptID = id
            detail.reset()
            output.reset()
        }
        await reload()
    }

    func reload() async {
        guard let id = attemptID else { return }
        async let body: Void = loadOutput(id)
        await detail.load { [client] in try await client.detail(id) }
        await body
    }

    func setPart(_ part: Part) async {
        self.part = part
        output.reset()
        if let id = attemptID { await loadOutput(id) }
    }

    private func loadOutput(_ id: String) async {
        await output.load { [client, part] in try await client.output(id, part: part.rawValue) }
    }
}
