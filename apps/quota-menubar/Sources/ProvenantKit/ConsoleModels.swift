import Foundation

// Wire shapes of the `console` reads. Only fields the console shows are decoded, and anything the CLI may leave out
// is optional, so an added field or a partial answer (a detail that is "unavailable") still decodes.

/// Where an attempt lives. Board and tree rows carry every part; a detail that was not found carries two.
public struct LaneID: Decodable, Hashable, Sendable {
    public let host: String?
    public let projectId: String?
    public let runId: String?
    public let taskId: String?
    public let attemptId: String
}

/// A route as requested or observed. Every part may be absent.
public struct Route: Decodable, Equatable, Sendable {
    public let kind: String?
    public let provider: String?
    public let model: String?
    public let effort: String?

    /// "model · effort", with the provider in front unless the model already names it.
    public var summary: String {
        var name = model ?? provider ?? ""
        if let provider, let model, !model.hasPrefix(provider + "/") { name = "\(provider)/\(model)" }
        return [name, effort ?? ""].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// The attempt lifecycle as the CLI reports it: `queued`, `starting`, `running`, `settling` or `terminal`.
public struct AttemptState: Decodable, Equatable, Sendable {
    public let state: String
    public let outcome: String?
    public let exitCode: Int64?
    public let finishedAt: String?
    public let reason: String?

    public var isTerminal: Bool { state == "terminal" }
}

public struct AttemptRecord: Decodable, Equatable, Sendable {
    public let attemptId: String
    public let title: String?
    public let state: AttemptState
    public let attemptNumber: UInt64?
    public let queuedAt: String?
    public let startedAt: String?
    public let requestedRoute: Route?
}

public struct LaneMetadata: Decodable, Equatable, Sendable {
    public let attempt: AttemptRecord
}

public struct LaneWait: Decodable, Equatable, Sendable {
    public let laneClass: String?
    public let waitingFor: String?
}

/// One row of the run board.
public struct BoardLane: Decodable, Equatable, Sendable, Identifiable {
    public let laneId: LaneID
    public let attention: Bool
    public let metadata: LaneMetadata
    public let wait: LaneWait?
    public let observationCoverage: String?
    public var id: String { laneId.attemptId }
    public var title: String { metadata.attempt.title ?? "Attempt \(laneId.attemptId.prefix(8))" }
}

public struct BoardCoverage: Decodable, Equatable, Sendable {
    public let runs: String
    public let nestedRuns: String?
    public let observations: String?
    public let chairs: String?
    public let scope: String?
}

/// `console board` (data version 3).
public struct Board: Decodable, Equatable, Sendable {
    public let coverage: BoardCoverage
    public let generation: String
    public let highWater: UInt64
    public let lanes: [BoardLane]
    public let nextCursor: String?
    public let observedAt: String?
    public let truncated: Bool
    public let snapshot: String?
}

public struct TreeChild: Decodable, Equatable, Sendable {
    public let kind: String
    public let attemptId: String
}

public struct TreeNode: Decodable, Equatable, Sendable, Identifiable {
    public let laneId: LaneID
    public let depth: Int
    public let metadata: LaneMetadata
    public let children: [TreeChild]?
    public var id: String { laneId.attemptId }
}

public struct TreeCoverage: Decodable, Equatable, Sendable {
    public let nestedRuns: String?
    public let runs: String?
}

/// `console tree --detail`: nested runs.
public struct Tree: Decodable, Equatable, Sendable {
    public let nodes: [TreeNode]
    public let truncated: Bool
    public let coverage: TreeCoverage?
}

public struct Artifact: Decodable, Equatable, Sendable, Identifiable {
    public let name: String
    public let bytes: UInt64?
    public let complete: Bool?
    public let digest: String?
    public let present: Bool
    public let redacted: Bool?
    public var id: String { name }
}

public struct Timings: Decodable, Equatable, Sendable {
    public let queuedAt: String?
    public let startedAt: String?
    public let finishedAt: String?
    public let providerMs: UInt64?
}

public struct Usage: Decodable, Equatable, Sendable {
    public let uncachedInput: UInt64?
    public let cacheRead: UInt64?
    public let cacheWrite: UInt64?
    public let outputExcludingReasoning: UInt64?
    public let reasoning: UInt64?
    public let contextBasisPoints: UInt64?

    /// Each reported count with its label, in reading order. A count the CLI did not report is left out, not zero.
    public var counts: [(label: String, value: UInt64)] {
        let all: [(String, UInt64?)] = [("Input", uncachedInput), ("Cache read", cacheRead), ("Cache write", cacheWrite),
                                        ("Output", outputExcludingReasoning), ("Reasoning", reasoning)]
        return all.compactMap { pair in pair.1.map { (label: pair.0, value: $0) } }
    }
}

public struct UsageCoverage: Decodable, Equatable, Sendable {
    public let known: [String]
    public let missing: [String]
}

public struct Trace: Decodable, Equatable, Sendable {
    public let actor: String?
    public let attempt: String?
    public let operation: String?
    public let parent: String?
    public let project: String?
    public let run: String?
}

public struct LinkedRequest: Decodable, Equatable, Sendable, Identifiable {
    public let id: String
    public let revision: UInt64?
    public let state: String?
    public let title: String?
}

public struct DetailCoverage: Decodable, Equatable, Sendable {
    public let linkedInboxRequests: String?
    public let nested: String?
    public let peer: String?
}

/// `console detail ID` (data version 1). `status` is `found`, `missing` or `unavailable`; only a found attempt has
/// the rest.
public struct AttemptDetail: Decodable, Equatable, Sendable {
    public let status: String
    public let reason: String?
    public let laneId: LaneID
    public let requestedRoute: Route?
    public let resolvedRoute: Route?
    public let appliedRoute: Route?
    public let observedRoute: Route?
    public let timings: Timings?
    public let usage: Usage?
    public let usageCoverage: UsageCoverage?
    public let trace: Trace?
    public let artifacts: [Artifact]?
    public let linkedInboxRequests: [LinkedRequest]?
    public let coverage: DetailCoverage?

    public var isFound: Bool { status == "found" }
}

public struct OverviewCounts: Decodable, Equatable, Sendable {
    public let needsYou: UInt64?
    public let running: UInt64?
    public let attention: UInt64?
}

public struct OverviewCoverage: Decodable, Equatable, Sendable {
    public let inbox: String?
    public let runs: String?
    public let nestedRuns: String?
    public let scope: String?

    /// What the overview could not see, as short phrases. Empty when nothing is missing.
    public var warnings: [String] {
        let parts: [(String, String?)] = [("Inbox", inbox), ("Runs", runs), ("Nested runs", nestedRuns)]
        return parts.compactMap { name, value in
            guard let value, value != "complete", value != "empty" else { return nil }
            return "\(name): \(value.replacingOccurrences(of: "_", with: " "))"
        }
    }
}

public struct NeedsYouEntry: Decodable, Equatable, Sendable, Identifiable {
    public let id: String
    public let title: String
    public let chairSessionId: String?
    public let projectId: String?
    public let projectLabel: String?
}

/// `console overview` (data version 2). A null count means the CLI could not count it.
/// An active or attention-needing lane in `console overview`. This is not a
/// board row: the overview carries only identity, label and coarse state.
public struct OverviewLane: Decodable, Equatable, Sendable, Identifiable {
    public let projectId: String
    public let projectLabel: String?
    public let runId: String
    public let taskId: String?
    public let attemptId: String
    public let taskLabel: String?
    /// `queued`, `starting`, `running`, `settling` or `terminal`.
    public let state: String
    public let attention: Bool
    public var id: String { attemptId }
    public var title: String { taskLabel ?? "Attempt \(attemptId.prefix(8))" }
}

public struct Overview: Decodable, Equatable, Sendable {
    public let counts: OverviewCounts
    public let coverage: OverviewCoverage
    public let needsYou: [NeedsYouEntry]
    public let lanes: [OverviewLane]
}

public struct ProjectEntry: Decodable, Equatable, Sendable, Identifiable {
    public struct CommonDir: Decodable, Equatable, Sendable { public let path: String }
    public let projectId: String
    public let label: String?
    public let commonDir: CommonDir?
    public var id: String { projectId }
}

/// `console projects` (data version 2).
public struct Projects: Decodable, Equatable, Sendable {
    public let projects: [ProjectEntry]
}

public struct SessionSummary: Decodable, Equatable, Sendable {
    public let now: String?
    public let next: String?
    public let risk: String?
    public let updatedAt: String?
    public let needsYou: [String]?
}

public struct SessionRecord: Decodable, Equatable, Sendable {
    public let hostSessionId: String
    public let projectId: String?
    public let createdAt: String?
    public let lastSavedAt: String?
}

public struct ChairSession: Decodable, Equatable, Sendable, Identifiable {
    public let role: String
    public let consistency: String?
    public let summaryStatus: String?
    public let summary: SessionSummary?
    public let record: SessionRecord
    public var id: String { record.hostSessionId }
}

/// `console sessions` (data version 2). `summary` is null when the CLI has none for a session.
public struct Sessions: Decodable, Equatable, Sendable {
    public let sessions: [ChairSession]
    public let nextCursor: String?
    public let truncated: Bool
    public let scanTruncated: Bool?
}

public struct SearchMatch: Decodable, Equatable, Sendable, Identifiable {
    public let laneId: LaneID
    public let part: String
    public let snippet: String
    public var id: String { "\(laneId.attemptId)/\(part)" }
}

/// `console search QUERY --part prompt|result`.
public struct SearchResults: Decodable, Equatable, Sendable {
    public let query: String
    public let part: String
    public let matches: [SearchMatch]
    public let nextCursor: String?
    public let truncated: Bool
}

/// The body of `provenant output ATTEMPT`: text, or base64 for bytes that are not UTF-8.
public struct AttemptOutput: Decodable, Equatable, Sendable {
    public struct Bytes: Decodable, Equatable, Sendable { public let encoding: String; public let data: String }
    public let attemptId: String?
    public let state: String?
    public let bytes: Bytes?

    /// The text, or nil when there is no body or it is binary.
    public var text: String? {
        guard let bytes, bytes.encoding == "utf8" else { return nil }
        return bytes.data
    }
}

// MARK: Quota

/// `quota show`: the accounts the menu agent also draws, reduced to what a table needs.
public struct QuotaShow: Decodable, Equatable, Sendable {
    public struct Scope: Decodable, Equatable, Sendable { public let adapter: String; public let accountKey: String; public let label: String }
    public struct Window: Decodable, Equatable, Sendable {
        public let pool: String
        public let usedBasisPoints: UInt64
        public let resetsUnixSeconds: Int64?
    }
    public struct Snapshot: Decodable, Equatable, Sendable { public let windows: [Window] }
    public struct Account: Decodable, Equatable, Sendable, Identifiable {
        public let account: Scope
        public let snapshot: Snapshot?
        public let status: String
        public let ageSeconds: UInt64?
        public let error: String?
        public var id: String { "\(account.adapter)/\(account.accountKey)" }
    }
    public let generatedUnixSeconds: Int64
    public let accounts: [Account]
}

/// `quota history`: dated readings per account.
public struct QuotaHistory: Decodable, Equatable, Sendable {
    public struct Reading: Decodable, Equatable, Sendable, Identifiable {
        public let observedUnixSeconds: Int64
        public let accountKey: String
        public let label: String
        public let provider: String
        public let status: String
        public let windows: [QuotaShow.Window]
        public var id: String { "\(provider)/\(accountKey)/\(observedUnixSeconds)" }
    }
    public let readings: [Reading]
    public let truncated: Bool
}

// MARK: Changes

/// The owner's change generations. A nil part is unknown, never zero.
public struct Generations: Decodable, Equatable, Sendable {
    public var epoch: UInt64?
    public var inbox: UInt64?
    public var board: UInt64?
    public var chair: UInt64?
    public var quota: UInt64?
    public var configuration: UInt64?
    public var runs: UInt64?

    public init() {}

    /// The JSON object `console changes --since` takes: every key present, unknown parts null.
    public var sinceArgument: String {
        func part(_ key: String, _ value: UInt64?) -> String { "\"\(key)\":\(value.map(String.init) ?? "null")" }
        return "{" + [part("epoch", epoch), part("inbox", inbox), part("board", board), part("chair", chair),
                      part("quota", quota), part("configuration", configuration), part("runs", runs)].joined(separator: ",") + "}"
    }
}

/// Which scopes moved. Each is true (moved), false (same) or nil (unknown, so reload).
public struct ChangedScopes: Decodable, Equatable, Sendable {
    public let board: Bool?
    public let chair: Bool?
    public let configuration: Bool?
    public let inbox: Bool?
    public let quota: Bool?
    public let runs: Bool?
}

/// `console changes` (data version 2).
public struct Changes: Decodable, Equatable, Sendable {
    public let changed: ChangedScopes
    public let generations: Generations
}
