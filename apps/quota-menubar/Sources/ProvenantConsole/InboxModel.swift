import Foundation
import Observation
import ProvenantKit

/// What the inbox window shows. Every read and write goes through `InboxClient`, so the model holds no store handle
/// and keeps no timer: it refreshes when told to.
@MainActor @Observable
final class InboxModel {
    private(set) var requests: [HumanRequest] = []
    private(set) var truncated = false
    private(set) var refreshing = false
    private(set) var sending = false
    private(set) var lastRefreshed: Date?
    /// The last refresh's failure; the previous list stays on screen.
    private(set) var refreshError: String?
    /// The answer, delivery and current state the CLI returned for a request this window answered or found stale.
    private(set) var results: [String: Result] = [:]
    var selection: String?

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

    var selected: HumanRequest? {
        selection.flatMap { id in requests.first { $0.id == id } ?? results[id]?.request }
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
            if selection == nil || selected == nil { selection = requests.first?.id }
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
                results[request.id] = Result(kind: .saved, message: Self.saved(receipt), request: request, item: item)
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

    static func saved(_ receipt: InboxReceipt) -> String {
        guard let delivery = receipt.delivery else { return "Answer saved (\(receipt.state))." }
        return "Answer saved (\(receipt.state)). \(deliveryLine(delivery))"
    }

    /// The delivery and acknowledgement state as the CLI returned it.
    static func deliveryLine(_ delivery: AnswerDelivery) -> String {
        var line = "Delivery \(delivery.state) by \(delivery.transport.replacingOccurrences(of: "_", with: " ")), \(delivery.attempts) \(delivery.attempts == 1 ? "attempt" : "attempts")"
        if let at = delivery.acknowledgedAt { line += ", acknowledged \(at)" }
        if let error = delivery.error { line += ", error: \(error)" }
        return line + "."
    }

    static func describe(_ error: Error) -> String { (error as? ClientError)?.message ?? error.localizedDescription }
}
