import Foundation
import ProvenantKit

/// The bounded local projection, without artifact bodies.
struct ConsoleOverview: Decodable, Equatable, Sendable {
    struct Request: Decodable, Equatable, Sendable {
        let id: String
        let projectLabel, title, taskLabel, createdAt, deadline: String?
    }
    struct Lane: Decodable, Equatable, Sendable {
        let projectLabel, taskLabel, attemptId, queuedAt: String?
        let state: String
        let attention: Bool
    }
    struct Counts: Decodable, Equatable, Sendable { let needsYou, running, attention: Int? }
    struct Coverage: Decodable, Equatable, Sendable { let scope, inbox, runs, nestedRuns: String }
    let needsYou: [Request]
    let lanes: [Lane]
    let counts: Counts
    let coverage: Coverage

    fileprivate struct InboxPage: Decodable, Sendable {
        struct Project: Decodable, Sendable { let label: String? }
        let items: [HumanRequest]
        let projects: [String: Project]?
        let truncated: Bool
        let nextCursor: String?
    }

    private struct Envelope: Decodable {
        let schemaVersion: UInt64
        let command, status: String
        let data: ConsoleOverview?
    }
    static func decode(_ data: Data) throws -> ConsoleOverview {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope = try decoder.decode(Envelope.self, from: data)
        guard envelope.schemaVersion == 1, envelope.command == "console overview", envelope.status == "ok",
              let overview = envelope.data else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "unexpected overview envelope"))
        }
        return overview
    }
}

extension CLI {
    /// Two bounded owner reads per refresh, with no per-row subprocesses.
    static func overview(_ cached: Launcher?) -> (Launcher?, Result<ConsoleOverview, Failure>) {
        guard let launcher = cached ?? resolve() else { return (nil, .failure(Failure(message: "provenant was not found"))) }
        do {
            let run = try exec(launcher.url, ["console", "overview", "--json"], path: launcher.path, limit: 20,
                               qos: QOS_CLASS_BACKGROUND)
            let overview = try CLIClient.decode(run, command: "console overview", as: ConsoleOverview.self)
            let inboxRun = try exec(launcher.url, ["inbox", "--all-projects", "--state", "open", "--limit", "100", "--json"],
                                    path: launcher.path, limit: 20, qos: QOS_CLASS_BACKGROUND)
            let inbox = try CLIClient.decode(inboxRun, command: "inbox", as: ConsoleOverview.InboxPage.self)
            let requests = inbox.items.filter(\.isOpen).map { item in
                ConsoleOverview.Request(id: item.id, projectLabel: item.projectId.flatMap { inbox.projects?[$0]?.label },
                                        title: item.request.title, taskLabel: nil, createdAt: item.createdAt,
                                        deadline: item.request.deadline)
            }
            let coverage = ConsoleOverview.Coverage(scope: overview.coverage.scope,
                inbox: inbox.truncated || inbox.nextCursor != nil ? "partial" : "complete",
                runs: overview.coverage.runs, nestedRuns: overview.coverage.nestedRuns)
            let counts = ConsoleOverview.Counts(needsYou: requests.count, running: overview.counts.running,
                                                attention: overview.counts.attention)
            return (launcher, .success(ConsoleOverview(needsYou: requests, lanes: overview.lanes, counts: counts, coverage: coverage)))
        } catch {
            return (launcher, .failure(Failure(message: (error as? ClientError)?.message ?? "Could not read console overview: \(error.localizedDescription)")))
        }
    }
}
