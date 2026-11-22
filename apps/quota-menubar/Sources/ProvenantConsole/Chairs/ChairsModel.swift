import Foundation
import Observation
import ProvenantDesign
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
    private let now: () -> Date

    init(client: ConsoleClient, now: @escaping () -> Date = { .now }) { self.client = client; self.now = now }

    func refresh() async {
        await reader.load(now: now) { [client] in
            async let sessions = client.sessions()
            async let projects = client.projects()
            let page = try await sessions
            return ChairsSnapshot(sessions: Self.latestPerSession(page.sessions), projects: try await projects.projects,
                                  truncated: page.truncated)
        }
    }

    /// A summary when the CLI has one; otherwise the record's project, session and save time. A bare "no summary"
    /// placeholder tells a supervisor nothing and repeats on every row of a host with no CLI summaries.
    nonisolated static func title(_ session: ChairSession, project: String?, now: Date) -> String {
        if let summary = session.summary, !summary.isEmpty { return summary }
        var parts: [String] = []
        if let project, !project.isEmpty { parts.append(project) }
        parts.append("Session \(session.record.hostSessionId.prefix(8))")
        if let saved = session.record.lastSavedAt { parts.append("saved \(Stamp.relative(saved, now: now))") }
        return parts.joined(separator: " · ")
    }

    /// The CLI can return several saved records for one host session. Show the most recently saved one only.
    nonisolated static func latestPerSession(_ sessions: [ChairSession]) -> [ChairSession] {
        var indices: [String: Int] = [:]
        var result: [ChairSession] = []
        for session in sessions {
            let id = session.record.hostSessionId
            guard let index = indices[id] else {
                indices[id] = result.count
                result.append(session)
                continue
            }
            if (session.record.lastSavedAt ?? "") > (result[index].record.lastSavedAt ?? "") { result[index] = session }
        }
        return result
    }
}
