import Foundation
import Observation
import ProvenantKit

struct ChairsSnapshot: Equatable, Sendable {
    let sessions: [ChairSession]
    let projects: [ProjectEntry]
    let truncated: Bool

    /// A project's name for a session: its registered label, else the last part of its directory.
    func projectName(_ id: String?) -> String? {
        guard let id, let project = projects.first(where: { $0.projectId == id }) else { return nil }
        return project.label ?? project.commonDir?.path.split(separator: "/").dropLast().last.map(String.init)
    }
}

/// Chairs and Projects: `console sessions` and `console projects`.
@MainActor @Observable
final class ChairsModel {
    let reader = Reader<ChairsSnapshot>()
    private let client: ConsoleClient

    init(client: ConsoleClient) { self.client = client }

    func refresh() async {
        await reader.load { [client] in
            async let sessions = client.sessions()
            async let projects = client.projects()
            let page = try await sessions
            return ChairsSnapshot(sessions: page.sessions, projects: try await projects.projects, truncated: page.truncated)
        }
    }
}
