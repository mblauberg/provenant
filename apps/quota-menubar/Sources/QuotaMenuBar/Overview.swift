import Foundation

/// The bounded local projection, without artifact bodies.
struct ConsoleOverview: Decodable, Equatable, Sendable {
    struct Request: Decodable, Equatable, Sendable {
        let id: String
        let projectLabel, title, taskLabel: String?
    }
    struct Lane: Decodable, Equatable, Sendable {
        let projectLabel, taskLabel: String?
        let state: String
        let attention: Bool
    }
    struct Counts: Decodable, Equatable, Sendable { let needsYou, running, attention: Int? }
    struct Coverage: Decodable, Equatable, Sendable { let scope, inbox, runs, nestedRuns: String }
    let needsYou: [Request]
    let lanes: [Lane]
    let counts: Counts
    let coverage: Coverage

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
    /// One bounded owner read per panel refresh, with no per-row subprocesses.
    static func overview(_ cached: Launcher?) -> (Launcher?, Result<ConsoleOverview, Failure>) {
        guard let launcher = cached ?? resolve() else { return (nil, .failure(Failure(message: "provenant was not found"))) }
        do {
            let run = try exec(launcher.url, ["console", "overview", "--json"], path: launcher.path, limit: 20,
                               qos: QOS_CLASS_BACKGROUND)
            guard run.status == 0, !run.timedOut, !run.signalled else {
                return (launcher, .failure(Failure(message: "Could not read console overview")))
            }
            return (launcher, .success(try ConsoleOverview.decode(run.out)))
        } catch {
            return (launcher, .failure(Failure(message: "Could not read console overview: \(error.localizedDescription)")))
        }
    }
}
