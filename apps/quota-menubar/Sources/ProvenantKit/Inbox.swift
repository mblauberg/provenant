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
    public let projectId: String?
    public let projectKey: String?
    /// The project's name, set by `InboxClient.list` from the host-wide listing; nil in a project's own listing.
    public var projectLabel: String?
    public let chairSessionId: String
    public let request: Draft
    public let state: String
    public let answerId: String?
    public let closureReason: String?

    public var isOpen: Bool { state == "open" }

    /// The project this request belongs to, for the list: its registered name, else the last part of its key.
    public var projectName: String? {
        projectLabel ?? projectKey.flatMap { $0.split(separator: "/").last.map(String.init) }
    }
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

/// Reply-text files for answers whose CLI call has not returned. Each is 0600 and is removed when its call returns.
/// `finish` gives outstanding calls a bounded grace at quit, then removes what is left, so quitting during a slow
/// answer leaves no copy of the reply text behind.
public final class ReplyFiles: @unchecked Sendable {
    public static let shared = ReplyFiles()
    private let lock = NSLock()
    private let outstanding = DispatchGroup()
    private var pending: Set<URL> = []

    public init() {}

    /// Writes `text` to a new file and tracks it until `release`.
    func write(_ text: String) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "provenant-answer-\(UUID().uuidString).txt")
        guard FileManager.default.createFile(atPath: url.path, contents: Data(text.utf8), attributes: [.posixPermissions: 0o600]) else {
            throw ClientError.launch("could not write the reply text")
        }
        lock.withLock { _ = pending.insert(url) }
        outstanding.enter()
        return url
    }

    /// Removes a reply file once the call that read it has returned.
    func release(_ url: URL) {
        let tracked = lock.withLock { pending.remove(url) != nil }
        try? FileManager.default.removeItem(at: url)
        if tracked { outstanding.leave() }
    }

    /// Waits up to `grace` for outstanding calls to return, then removes the reply files of any still running.
    public func finish(within grace: DispatchTimeInterval) {
        _ = outstanding.wait(timeout: .now() + grace)
        let urls = lock.withLock { () -> Set<URL> in
            let all = pending
            pending = []
            return all
        }
        // Each removed file leaves the group, so a late `release` finds nothing tracked and leaves it alone.
        for url in urls {
            try? FileManager.default.removeItem(at: url)
            outstanding.leave()
        }
    }
}

/// The inbox through the CLI. Answers always say `--channel console` and the revision the person saw.
public struct InboxClient: Sendable {
    public let cli: CLIClient
    public let replies: ReplyFiles
    public init(cli: CLIClient = CLIClient(), replies: ReplyFiles = .shared) { self.cli = cli; self.replies = replies }

    private struct Listed: Decodable, Sendable {
        let items: [HumanRequest]
        let nextCursor: String?
        let truncated: Bool
        let coverage: String
        /// Present when the CLI listed the whole host (no project around it): each project's name and path.
        let projects: [String: Project]?
    }
    private struct Project: Decodable, Sendable { let label: String? }
    private struct Single: Decodable, Sendable { let item: InboxItem }

    /// Open requests (or every request) across pages, in the CLI's order. Run outside a project, the CLI lists the
    /// open requests of every project on the host, and each carries its project's name.
    public func list(includeClosed: Bool = false, pageSize: Int = 50, maxPages: Int = 10) async throws -> Page<HumanRequest> {
        try await collectPages(maxPages: maxPages) { cursor in
            var arguments = ["inbox", "--state", includeClosed ? "all" : "open", "--limit", String(pageSize)]
            if let cursor { arguments += ["--cursor", cursor] }
            let listed = try await cli.call(arguments, as: Listed.self)
            let items = listed.items.map { request -> HumanRequest in
                var request = request
                request.projectLabel = request.projectId.flatMap { listed.projects?[$0]?.label }
                return request
            }
            return Page(items: items, nextCursor: listed.nextCursor, truncated: listed.truncated)
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
        defer { if let file { replies.release(file) } }
        if let text {
            // The CLI reads reply text from a file, so the text never appears in an argument list.
            let url = try replies.write(text)
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
