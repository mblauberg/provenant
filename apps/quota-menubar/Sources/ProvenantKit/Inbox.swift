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
    /// The host that owns the request. Every row carries it, local rows included.
    public let ownerHost: String?
    /// The project's name, set by `InboxClient.list` from the host-wide listing; nil in a project's own listing.
    public var projectLabel: String?
    /// Set by `InboxClient.list` for a row a peer host owns: the peer's host ID. Nil for this host's own rows.
    public var peerHost: String?
    /// Set with `peerHost`: the peer's configured label, for display.
    public var peerLabel: String?
    public let chairSessionId: String
    public let request: Draft
    public let state: String
    public let answerId: String?
    public let closureReason: String?

    public var isOpen: Bool { state == "open" }

    /// What the CLI needs to reach a peer's request from this host; nil for a local row.
    public var peerRoute: PeerRoute? {
        guard let peerHost, let projectKey else { return nil }
        return PeerRoute(host: peerHost, projectKey: projectKey)
    }

    /// The project this request belongs to, for the list: its registered name, else the last part of its key.
    public var projectName: String? {
        projectLabel ?? projectKey.flatMap { $0.split(separator: "/").last.map(String.init) }
    }
}

/// The owner host and project key a peer's request is shown and answered through (`--host`, `--project-key`).
public struct PeerRoute: Equatable, Sendable {
    public let host: String
    public let projectKey: String
    public init(host: String, projectKey: String) { self.host = host; self.projectKey = projectKey }
    var arguments: [String] { ["--host", host, "--project-key", projectKey] }
}

/// What one peer host contributed to a listing, from `data.peers`. `state` stays a string, so a newer CLI's state
/// still decodes; `kind` reads the ones this app knows.
public struct PeerCoverage: Decodable, Equatable, Sendable, Identifiable {
    public enum Kind: Equatable, Sendable { case complete, partial, unreachable, unsupported, other(String) }

    public let host: String
    public let label: String
    public let state: String
    public let included: Int
    public let invalidRows: Int
    public let duplicateRows: Int
    public let truncated: Bool
    public let reason: String?

    public var id: String { host }

    public init(host: String, label: String, state: String, included: Int = 0, invalidRows: Int = 0,
                duplicateRows: Int = 0, truncated: Bool = false, reason: String? = nil) {
        self.host = host; self.label = label; self.state = state; self.included = included
        self.invalidRows = invalidRows; self.duplicateRows = duplicateRows; self.truncated = truncated; self.reason = reason
    }

    private enum Keys: String, CodingKey { case host, label, state, included, invalidRows, duplicateRows, truncated, reason }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        host = try c.decode(String.self, forKey: .host)
        state = try c.decode(String.self, forKey: .state)
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? host
        included = try c.decodeIfPresent(Int.self, forKey: .included) ?? 0
        invalidRows = try c.decodeIfPresent(Int.self, forKey: .invalidRows) ?? 0
        duplicateRows = try c.decodeIfPresent(Int.self, forKey: .duplicateRows) ?? 0
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
    }

    public var kind: Kind {
        switch state {
        case "complete": .complete
        case "partial": .partial
        case "unreachable": .unreachable
        case "unsupported": .unsupported
        default: .other(state)
        }
    }

    /// True when the peer gave no page this time (refused, invalid page, duplicate host, unknown project, or
    /// unreachable), as opposed to a page with gaps. Its rows are not fresh, whatever the host had before.
    public var contributedNoPage: Bool {
        switch kind {
        case .unreachable: true
        case .partial: reason != nil && included == 0
        // A state this app does not know counts only when it says why, as a partial one does; one that gives
        // neither a page nor a reason is not trusted to have replaced the host's rows.
        case .other: reason != nil && included == 0
        case .complete, .unsupported: false
        }
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

    /// Seconds a first-page listing that asks for peers may run: clearly above `limits.peer_seconds`.
    static let peerListLimit: Double = 60

    struct Listed: Decodable, Sendable {
        let items: [HumanRequest]
        let nextCursor: String?
        let truncated: Bool
        let coverage: String
        /// Present when the CLI listed the whole host: each project's name and path. A peer's projects are keyed
        /// `"<host>:<project_id>"`.
        let projects: [String: Project]?
        /// Present on a first page listed with `--hosts`: what each peer contributed.
        let peers: [PeerCoverage]?
    }
    struct Project: Decodable, Sendable { let label: String?; let host: String? }
    private struct Single: Decodable, Sendable { let item: InboxItem }

    /// Requests across pages, in the CLI's order, from every registered project on the host.
    public func list(includeClosed: Bool = false, pageSize: Int = 50, maxPages: Int = 10) async throws -> Page<HumanRequest> {
        return try await collectPages(maxPages: maxPages) { cursor in
            var arguments = ["inbox", "--all-projects", "--state", includeClosed ? "all" : "open", "--limit", String(pageSize)]
            // Peers are read on the first page only; a cursor page is this host's rows.
            if let cursor { arguments += ["--cursor", cursor] }
            let withPeers = cursor == nil
            // The peer fan-out waits up to the configured peer deadline (30 s by default) after the local listing, so
            // a call that asks for peers gets a longer budget than that; otherwise an unreachable peer would time
            // out the call and hide the local rows.
            let listed = try await cli.call(arguments + (withPeers ? ["--hosts", "all"] : []), command: "inbox",
                                            limit: withPeers ? Self.peerListLimit : nil, as: Listed.self)
            return Page(items: Self.attribute(listed), nextCursor: listed.nextCursor, truncated: listed.truncated, peers: listed.peers)
        }
    }

    /// Names each row's project, and marks the rows a peer owns with that peer's host and label.
    static func attribute(_ listed: Listed) -> [HumanRequest] {
        let labels = Dictionary(listed.peers?.map { ($0.host, $0.label) } ?? [], uniquingKeysWith: { first, _ in first })
        return listed.items.map { request -> HumanRequest in
            var request = request
            let peerKey = request.ownerHost.flatMap { host in request.projectId.map { "\(host):\($0)" } }
            let peer = request.ownerHost.flatMap { host in
                labels[host] != nil || peerKey.flatMap({ listed.projects?[$0] }) != nil ? host : nil
            }
            if let peer {
                request.peerHost = peer
                request.peerLabel = labels[peer] ?? peerKey.flatMap { listed.projects?[$0]?.host } ?? peer
                request.projectLabel = peerKey.flatMap { listed.projects?[$0]?.label }
            } else {
                request.projectLabel = request.projectId.flatMap { listed.projects?[$0]?.label }
            }
            return request
        }
    }

    /// One request. A peer's request is read through its owner host and project key.
    public func item(_ id: String, route: PeerRoute? = nil) async throws -> InboxItem {
        try await cli.call(["inbox", "--all-projects", id] + (route?.arguments ?? []), command: "inbox", as: Single.self).item
    }

    /// `provenant answer`. A `conflict` refusal reads the request again and returns it as `.stale`.
    public func answer(_ id: String, action: AnswerAction, option: String? = nil, text: String? = nil,
                       expectRevision: UInt64, route: PeerRoute? = nil) async throws -> AnswerOutcome {
        var arguments = ["answer", "--all-projects", id] + (route?.arguments ?? [])
        arguments += ["--action", action.rawValue, "--expect-revision", String(expectRevision), "--channel", "console"]
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
            return .saved(try await cli.call(arguments, command: "answer", as: InboxReceipt.self))
        } catch let error as ClientError where error.code == "conflict" {
            return .stale(try await item(id, route: route))
        }
    }
}
