import Foundation

/// Typed reads of the console's CLI surface. Each method names its command, so a reply to a different command, or
/// at another envelope version, is refused rather than half read. Nothing here writes.
public struct ConsoleClient: Sendable {
    public let cli: CLIClient
    public init(cli: CLIClient = CLIClient()) { self.cli = cli }

    public func overview() async throws -> Overview {
        try await cli.call(["console", "overview"], command: "console overview")
    }

    /// One page of the run board. `all` includes retained terminal attempts.
    public func board(all: Bool = false, limit: Int = 50, cursor: String? = nil) async throws -> Board {
        var arguments = ["console", "board", "--limit", String(limit)]
        if all { arguments.append("--all") }
        if let cursor { arguments += ["--cursor", cursor] }
        return try await cli.call(arguments, command: "console board")
    }

    /// Nested runs with their records, for the Runs tree.
    public func tree(maxNodes: Int = 100) async throws -> Tree {
        try await cli.call(["console", "tree", "--detail", "--max-nodes", String(maxNodes)], command: "console tree")
    }

    public func detail(_ attempt: String) async throws -> AttemptDetail {
        try await cli.call(["console", "detail", attempt], command: "console detail")
    }

    /// An attempt's `result` or `prompt`, at most `maxBytes` of it.
    public func output(_ attempt: String, part: String = "result", maxBytes: Int = 16_384) async throws -> AttemptOutput {
        try await cli.call(["output", attempt, "--part", part, "--max-bytes", String(maxBytes)], command: "output")
    }

    public func sessions(limit: Int = 50, cursor: String? = nil) async throws -> Sessions {
        var arguments = ["console", "sessions", "--limit", String(limit)]
        if let cursor { arguments += ["--cursor", cursor] }
        return try await cli.call(arguments, command: "console sessions")
    }

    public func projects() async throws -> Projects {
        try await cli.call(["console", "projects"], command: "console projects")
    }

    /// Local prompt or result content. `part` is `prompt` or `result`.
    public func search(_ query: String, part: String, limit: Int = 50, cursor: String? = nil) async throws -> SearchResults {
        var arguments = ["console", "search", "--part", part, "--limit", String(limit)]
        if let cursor { arguments += ["--cursor", cursor] }
        arguments += ["--", query]
        return try await cli.call(arguments, command: "console search")
    }

    public func quota() async throws -> QuotaShow {
        try await cli.call(["quota", "show"], command: "quota show")
    }

    public func quotaHistory(limit: Int = 300) async throws -> QuotaHistory {
        try await cli.call(["quota", "history", "--limit", String(limit)], command: "quota history")
    }

    /// The change generations against `since`; without it every scope reads as unknown.
    public func changes(since: Generations? = nil) async throws -> Changes {
        var arguments = ["console", "changes"]
        if let since { arguments += ["--since", since.sinceArgument] }
        return try await cli.call(arguments, command: "console changes")
    }
}
