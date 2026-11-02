import Foundation
import Observation
import ProvenantDesign
import ProvenantKit

/// What the inbox window shows. Every read and write goes through `InboxClient`, so the model holds no store handle
/// and keeps no timer: it refreshes when told to.
@MainActor @Observable
final class InboxModel {
    struct ProjectGroup: Identifiable, Equatable, Sendable {
        let id: String
        let name: String
        let requests: [HumanRequest]
        var count: Int { requests.count }
    }

    private(set) var requests: [HumanRequest] = []
    /// What each peer host contributed to the last listing. Empty when the CLI reported no peers.
    private(set) var peerCoverage: [PeerCoverage] = []
    /// When a peer's rows were last read, for each peer whose rows are being kept because it did not answer.
    private(set) var staleSince: [String: Date] = [:]
    /// When each peer last contributed a page, whether or not its rows are still shown.
    private var lastGood: [String: Date] = [:]
    private(set) var truncated = false
    private(set) var refreshing = false
    private(set) var sending = false
    private(set) var lastRefreshed: Date?
    /// The last refresh's failure; the previous list stays on screen.
    private(set) var refreshError: String?
    /// The answer, delivery and current state the CLI returned for a request this window answered or found stale.
    private(set) var results: [String: Result] = [:]
    private(set) var scope: InboxScope = .all
    private(set) var selectionState = InboxSelection()

    /// The selected request's ID. The list binds to this.
    var selection: String? {
        get { selectionState.id }
        set { selectionState.id = newValue }
    }

    /// The sidebar's search. It narrows the current scope and never changes the counts.
    var searchText = "" {
        didSet {
            guard searchText != oldValue else { return }
            reconcileSelection()
        }
    }

    /// True while the system search field is open. Navigation keys stand aside for typing.
    var searching = false
    /// True while the reply field has focus, so j and k type rather than move.
    var isTypingReply = false

    /// What the person has typed or chosen for a request. It survives switching between requests in one window.
    struct Draft: Equatable {
        var optionID: String?
        var reply = ""
    }

    private(set) var drafts: [String: Draft] = [:]

    struct Result: Equatable {
        enum Kind: Equatable { case saved, stale, failed }
        let kind: Kind
        let message: String
        /// The request as this window last knew it. It outlives the open list, so the result stays reachable.
        let request: HumanRequest
        /// The item as the CLI returned it after the answer, when that read succeeded.
        let item: InboxItem?
    }

    private let client: InboxClient
    private let now: () -> Date

    init(client: InboxClient = InboxClient(), now: @escaping () -> Date = { .now }) {
        self.client = client
        self.now = now
    }

    /// The clock the rows use for relative ages. It advances on each refresh, never on its own.
    var displayNow: Date { now() }

    var query: String { searchText.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// Open requests in the current scope, narrowed by the search text.
    var visibleRequests: [HumanRequest] {
        let scoped = switch scope {
        case .filter(let filter): requests.filter(filter.matches)
        case .project(let project): requests.filter { Self.projectID($0) == project }
        }
        guard !query.isEmpty else { return scoped }
        return scoped.filter { matchesSearch($0, query) }
    }

    /// Every project with open requests, in name order, with its open count.
    var projectGroups: [ProjectGroup] {
        Dictionary(grouping: requests, by: Self.projectID)
            .map { id, requests in
                ProjectGroup(id: id, name: requests.first.map(Self.projectName) ?? "No project", requests: requests)
            }
            .sorted { left, right in
                let order = left.name.localizedStandardCompare(right.name)
                return order == .orderedSame ? left.id < right.id : order == .orderedAscending
            }
    }

    func count(for filter: InboxFilter) -> Int { requests.lazy.filter(filter.matches).count }

    var selected: HumanRequest? {
        selection.flatMap { id in visibleRequests.first { $0.id == id } ?? results[id]?.request }
    }

    /// Switches the middle list to a filter or project and keeps the selection inside it.
    func show(_ scope: InboxScope) {
        guard scope != self.scope else { return }
        self.scope = scope
        reconcileSelection()
    }

    /// Opens the system search field. The view binds its presentation to `searching`.
    func beginSearch() { searching = true }

    /// Moves the selection through the visible list. It stops at each end, as a list should.
    func moveSelection(by offset: Int) { selectionState.move(by: offset, in: visibleRequests.map(\.id)) }

    func selectNext() { moveSelection(by: 1) }
    func selectPrevious() { moveSelection(by: -1) }

    /// The draft for a request, with the recommended option chosen until the person picks another.
    func draft(for request: HumanRequest) -> Draft {
        var draft = drafts[request.id] ?? Draft()
        if let chosen = draft.optionID, !request.request.options.contains(where: { $0.id == chosen }) {
            draft.optionID = request.request.recommendedOptionId
        } else if draft.optionID == nil {
            draft.optionID = request.request.recommendedOptionId
        }
        return draft
    }

    func setOption(_ optionID: String?, for request: HumanRequest) {
        var draft = draft(for: request)
        draft.optionID = optionID
        drafts[request.id] = draft
    }

    func setReply(_ text: String, for request: HumanRequest) {
        var draft = draft(for: request)
        draft.reply = text
        drafts[request.id] = draft
    }

    func reply(for request: HumanRequest) -> String { draft(for: request).reply }

    /// Picks an option by its 1-based position, for the number keys. Returns false when there is no such option.
    @discardableResult
    func chooseOption(number: Int, for request: HumanRequest) -> Bool {
        let options = request.request.options
        guard number >= 1, number <= options.count else { return false }
        setOption(options[number - 1].id, for: request)
        return true
    }

    /// Reads the open inbox. One read at a time; a refresh asked for during one is dropped, not queued.
    func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        do {
            let page = try await client.list()
            apply(page)
            truncated = page.truncated
            refreshError = nil
            lastRefreshed = now()
            reconcileSelection(keepingResult: true)
        } catch {
            refreshError = Self.describe(error)
        }
    }

    /// Takes a fresh listing. A peer that gave no page keeps the rows it last gave, marked stale; a peer that gave
    /// a page (complete, or partial with gaps) replaces them; an unsupported or unlisted peer's rows are dropped.
    private func apply(_ page: Page<HumanRequest>) {
        let at = now()
        var merged = page.items
        var stale: [String: Date] = [:]
        var good: [String: Date] = [:]
        for peer in page.peers ?? [] where peer.kind != .unsupported {
            if peer.contributedNoPage {
                let kept = requests.filter { $0.peerHost == peer.host }
                good[peer.host] = lastGood[peer.host]
                if !kept.isEmpty {
                    merged += kept
                    stale[peer.host] = lastGood[peer.host] ?? lastRefreshed ?? at
                }
            } else {
                good[peer.host] = at
            }
        }
        requests = merged
        peerCoverage = page.peers ?? []
        staleSince = stale
        lastGood = good
    }

    /// When a request's host was last read, if the request is a kept row from a peer that is not answering now.
    func staleSince(for request: HumanRequest) -> Date? { request.peerHost.flatMap { staleSince[$0] } }

    /// One line for each peer that did not give a complete page, in the CLI's order.
    var peerNotes: [PeerNote] {
        peerCoverage.compactMap { peer in
            let kept = requests.lazy.filter { $0.peerHost == peer.host }.count
            return Self.note(for: peer, kept: kept, since: staleSince[peer.host], now: displayNow)
                .map { PeerNote(id: peer.host, text: $0) }
        }
    }

    struct PeerNote: Identifiable, Equatable, Sendable {
        let id: String
        let text: String
    }

    /// The coverage line for a peer, or nil when it listed everything.
    static func note(for peer: PeerCoverage, kept: Int, since: Date?, now: Date) -> String? {
        let label = peer.label
        func stale() -> String {
            guard kept > 0 else { return " Its requests are not shown." }
            let when = since.map { " since \(Stamp.relative($0, now: now))" } ?? ""
            return " Showing \(kept) from its last read, stale\(when)."
        }
        switch peer.kind {
        case .complete:
            return nil
        case .unsupported:
            return "Update \(label): it is too old to list requests here."
        case .unreachable:
            return "\(label) is unreachable\(peer.reason == "timeout" ? " (timed out)" : "")." + stale()
        case .partial where peer.contributedNoPage:
            return "\(label) gave no requests: \(reasonText(peer.reason))." + stale()
        case .partial:
            var parts: [String] = []
            if peer.truncated { parts.append("\(peer.included)+ requests on \(label); more are not loaded") }
            let skipped = peer.invalidRows + peer.duplicateRows
            if skipped > 0 { parts.append("\(skipped) \(skipped == 1 ? "row" : "rows") from \(label) could not be used") }
            return (parts.isEmpty ? "\(label) gave a partial list" : parts.joined(separator: "; ")) + "."
        case .other(let state):
            return "\(label): coverage is \(state)."
        }
    }

    private static func reasonText(_ reason: String?) -> String {
        switch reason {
        case "project_unknown": "it knows none of this Mac’s projects"
        case "limit_refused": "it refused the page size"
        case "invalid_request": "it refused the request"
        case "invalid_page": "it sent an unreadable page"
        case "duplicate_host": "it is configured twice"
        case .some(let other): other.replacingOccurrences(of: "_", with: " ")
        case nil: "no reason given"
        }
    }

    /// Answers with the revision the person saw. A stale revision writes nothing and shows the current state.
    func answer(_ request: HumanRequest, action: AnswerAction, option: String? = nil, text: String? = nil) async {
        guard !sending else { return }
        sending = true
        defer { sending = false }
        do {
            switch try await client.answer(request.id, action: action, option: option, text: text,
                                          expectRevision: request.revision, route: request.peerRoute) {
            case .saved(let receipt):
                // A failed read leaves the item out, but the receipt still says what was saved and delivered.
                let item = try? await client.item(request.id, route: request.peerRoute)
                results[request.id] = Result(kind: .saved, message: saved(receipt), request: request, item: item)
            case .stale(let current):
                results[request.id] = Result(
                    kind: .stale,
                    message: "This request changed since you opened it (now revision \(current.request.revision), \(current.request.state)). Nothing was sent.",
                    request: current.request,
                    item: current)
            }
        } catch {
            results[request.id] = Result(kind: .failed, message: Self.describe(error), request: request, item: nil)
        }
        await refresh()
    }

    private func reconcileSelection(keepingResult: Bool = false) {
        selectionState.reconcile(visible: visibleRequests.map(\.id), pinned: Set(results.keys), keepPinned: keepingResult)
    }

    private func matchesSearch(_ request: HumanRequest, _ query: String) -> Bool {
        let haystack = [
            request.request.title,
            request.request.body,
            request.request.kind,
            request.projectName ?? "",
            request.peerLabel ?? "",
            request.chairSessionId,
            request.request.options.map { "\($0.label) \($0.description)" }.joined(separator: " "),
        ].joined(separator: "\n")
        return haystack.localizedCaseInsensitiveContains(query)
    }

    /// Projects are told apart by host as well as ID, since two hosts can number their projects alike.
    static func projectID(_ request: HumanRequest) -> String {
        let id = request.projectId ?? request.projectName ?? ""
        return request.peerHost.map { "\($0):\(id)" } ?? id
    }

    static func projectName(_ request: HumanRequest) -> String {
        request.projectDisplayName ?? "No project"
    }

    func saved(_ receipt: InboxReceipt) -> String {
        guard let delivery = receipt.delivery else { return "Answer saved (\(receipt.state))." }
        return "Answer saved (\(receipt.state)). \(Self.deliveryLine(delivery, now: displayNow))"
    }

    /// The delivery and acknowledgement state as the CLI returned it.
    static func deliveryLine(_ delivery: AnswerDelivery, now: Date) -> String {
        var line = "Delivery \(delivery.state) by \(delivery.transport.replacingOccurrences(of: "_", with: " ")), \(delivery.attempts) \(delivery.attempts == 1 ? "attempt" : "attempts")"
        if let at = delivery.acknowledgedAt { line += ", acknowledged \(Stamp.relative(at, now: now))" }
        if let error = delivery.error { line += ", error: \(error)" }
        return line + "."
    }

    static func describe(_ error: Error) -> String { (error as? ClientError)?.message ?? error.localizedDescription }
}
