import ProvenantKit

/// The quiet smart filters in the sidebar. Each one is a view of the same open requests.
enum InboxFilter: String, CaseIterable, Identifiable, Sendable {
    case all, blocking, decisions, questions, assumptions

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: "All"
        case .blocking: "Blocking"
        case .decisions: "Decisions"
        case .questions: "Questions"
        case .assumptions: "Assumptions"
        }
    }

    var symbol: String {
        switch self {
        case .all: "tray"
        case .blocking: "exclamationmark.octagon"
        case .decisions: "arrow.triangle.branch"
        case .questions: "questionmark.circle"
        case .assumptions: "lightbulb"
        }
    }

    func matches(_ request: HumanRequest) -> Bool {
        switch self {
        case .all: true
        case .blocking: request.request.blocking
        case .decisions: request.kind == .decision
        case .questions: request.kind == .question
        case .assumptions: request.kind == .assumption
        }
    }
}

/// What the request list shows: one smart filter or one project.
enum InboxScope: Hashable, Sendable {
    case filter(InboxFilter)
    case project(String)

    static let all = InboxScope.filter(.all)
}

/// The selected request, and how it moves. A value type, so the rules are testable without a window.
struct InboxSelection: Equatable, Sendable {
    var id: String?

    /// Moves through `visible`, stopping at each end. With nothing selected, forward picks the first and back the last.
    mutating func move(by offset: Int, in visible: [String]) {
        guard !visible.isEmpty else { id = nil; return }
        guard let current = id, let index = visible.firstIndex(of: current) else {
            id = offset < 0 ? visible.last : visible.first
            return
        }
        id = visible[min(max(index + offset, 0), visible.count - 1)]
    }

    /// Keeps the selection if it is still listed, or `pinned` (a request whose result is on screen) when asked;
    /// otherwise falls to the first listed request.
    mutating func reconcile(visible: [String], pinned: Set<String>, keepPinned: Bool) {
        if let id {
            if visible.contains(id) { return }
            if keepPinned, pinned.contains(id) { return }
        }
        id = visible.first
    }
}

/// The request kinds the CLI names. A newer CLI's kind is kept as it came.
enum RequestKind: Equatable, Sendable {
    case decision, question, assumption
    case other(String)

    init(_ raw: String) {
        switch raw.lowercased() {
        case "decision": self = .decision
        case "question": self = .question
        case "assumption": self = .assumption
        default: self = .other(raw)
        }
    }

    var title: String {
        switch self {
        case .decision: "Decision"
        case .question: "Question"
        case .assumption: "Assumption"
        case .other(let raw): raw.capitalized
        }
    }
}

extension HumanRequest {
    var kind: RequestKind { RequestKind(request.kind) }
}
