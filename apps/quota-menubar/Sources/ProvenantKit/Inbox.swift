import Foundation

/// A human request, answer and delivery as `provenant inbox --json` reports them. Strings stay strings so a newer
/// CLI's states still decode.
public struct HumanRequest: Decodable, Equatable, Sendable, Identifiable {
    public struct Option: Decodable, Equatable, Sendable, Identifiable {
        public let id: String
        public let label: String
        public let description: String
    }
    public struct Draft: Decodable, Equatable, Sendable {
        public let kind: String
        public let title: String
        public let body: String
        public let blocking: Bool
        public let options: [Option]
        public let recommendedOptionId: String?
        public let proceedingWith: String?
        public let deadline: String?
    }
    public let id: String
    public let revision: UInt64
    public let createdAt: String
    public let updatedAt: String
    public let projectKey: String?
    public let chairSessionId: String
    public let request: Draft
    public let state: String
    public let answerId: String?
    public let closureReason: String?

    public var isOpen: Bool { state == "open" }
}

public struct HumanAnswer: Decodable, Equatable, Sendable {
    public let id: String
    public let requestId: String
    public let requestRevision: UInt64
    public let action: String
    public let optionId: String?
    public let text: String?
    public let channel: String
    public let answeredAt: String
}

public struct AnswerDelivery: Decodable, Equatable, Sendable {
    public let state: String
    public let transport: String
    public let attempts: UInt64
    public let lastAttemptAt: String?
    public let acknowledgedAt: String?
    public let error: String?
}

public struct InboxItem: Decodable, Equatable, Sendable {
    public let request: HumanRequest
    public let answer: HumanAnswer?
    public let delivery: AnswerDelivery?
}

public struct InboxReceipt: Decodable, Equatable, Sendable {
    public let operationId: String
    public let requestId: String
    public let revision: UInt64
    public let state: String
    public let answerId: String?
    public let delivery: AnswerDelivery?
}

public enum AnswerAction: String, Sendable { case choose, reply, confirm, override }

/// What answering did.
public enum AnswerOutcome: Equatable, Sendable {
    /// The answer was saved; the receipt carries the delivery state as returned.
    case saved(InboxReceipt)
    /// The request moved on since it was viewed; nothing was written. `current` is its state now.
    case stale(InboxItem)
}

/// The inbox through the CLI. Answers always say `--channel console` and the revision the person saw.
public struct InboxClient: Sendable {
    public let cli: CLIClient
    public init(cli: CLIClient = CLIClient()) { self.cli = cli }

    private struct Listed: Decodable, Sendable {
        let items: [HumanRequest]
        let nextCursor: String?
        let truncated: Bool
        let coverage: String
    }
    private struct Single: Decodable, Sendable { let item: InboxItem }

    /// Open requests (or every request) across pages, in the CLI's order.
    public func list(includeClosed: Bool = false, pageSize: Int = 50, maxPages: Int = 10) async throws -> Page<HumanRequest> {
        try await collectPages(maxPages: maxPages) { cursor in
            var arguments = ["inbox", "--state", includeClosed ? "all" : "open", "--limit", String(pageSize)]
            if let cursor { arguments += ["--cursor", cursor] }
            let listed = try await cli.call(arguments, as: Listed.self)
            return Page(items: listed.items, nextCursor: listed.nextCursor, truncated: listed.truncated)
        }
    }

    public func item(_ id: String) async throws -> InboxItem {
        try await cli.call(["inbox", id], as: Single.self).item
    }

    /// `provenant answer`. A `conflict` refusal reads the request again and returns it as `.stale`.
    public func answer(_ id: String, action: AnswerAction, option: String? = nil, text: String? = nil,
                       expectRevision: UInt64) async throws -> AnswerOutcome {
        var arguments = ["answer", id, "--action", action.rawValue, "--expect-revision", String(expectRevision), "--channel", "console"]
        if let option { arguments += ["--option", option] }
        var file: URL?
        defer { file.map { try? FileManager.default.removeItem(at: $0) } }
        if let text {
            // The CLI reads reply text from a file, so the text never appears in an argument list.
            let url = FileManager.default.temporaryDirectory.appending(path: "provenant-answer-\(UUID().uuidString).txt")
            guard FileManager.default.createFile(atPath: url.path, contents: Data(text.utf8), attributes: [.posixPermissions: 0o600]) else {
                throw ClientError.launch("could not write the reply text")
            }
            file = url
            arguments += ["--text-file", url.path]
        }
        do {
            return .saved(try await cli.call(arguments, as: InboxReceipt.self))
        } catch let error as ClientError where error.code == "conflict" {
            return .stale(try await item(id))
        }
    }
}
