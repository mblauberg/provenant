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
            requests = page.items
            truncated = page.truncated
            refreshError = nil
            lastRefreshed = now()
            reconcileSelection(keepingResult: true)
        } catch {
            refreshError = Self.describe(error)
        }
    }

    /// Answers with the revision the person saw. A stale revision writes nothing and shows the current state.
    func answer(_ request: HumanRequest, action: AnswerAction, option: String? = nil, text: String? = nil) async {
        guard !sending else { return }
        sending = true
        defer { sending = false }
        do {
            switch try await client.answer(request.id, action: action, option: option, text: text, expectRevision: request.revision) {
            case .saved(let receipt):
                // A failed read leaves the item out, but the receipt still says what was saved and delivered.
                let item = try? await client.item(request.id)
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
            request.chairSessionId,
            request.request.options.map { "\($0.label) \($0.description)" }.joined(separator: " "),
        ].joined(separator: "\n")
        return haystack.localizedCaseInsensitiveContains(query)
    }

    static func projectID(_ request: HumanRequest) -> String {
        request.projectId ?? request.projectName ?? ""
    }

    static func projectName(_ request: HumanRequest) -> String {
        request.projectName ?? "No project"
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
