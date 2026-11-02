import Foundation
import Observation
import ProvenantKit

/// The board so far: every page read, and where the next one starts.
struct RunsPage: Equatable, Sendable {
    var lanes: [BoardLane]
    var nextCursor: String?
    var coverage: BoardCoverage
    var observedAt: String?
}

/// The Runs screen: the paged board, or the nested-run tree.
@MainActor @Observable
final class RunsModel {
    enum Mode: String, CaseIterable, Identifiable, Sendable {
        case list, tree
        var id: String { rawValue }
        var title: String { self == .list ? "List" : "Tree" }
        var symbol: String { self == .list ? "list.bullet" : "list.bullet.indent" }
    }

    let board = Reader<RunsPage>()
    let tree = Reader<Tree>()
    var mode: Mode = .list
    /// Includes retained terminal attempts.
    var includeFinished = false {
        didSet { if includeFinished != oldValue { Task { await refresh() } } }
    }
    var selection: String?
    private(set) var loadingMore = false
    private(set) var moreError: String?
    private let client: ConsoleClient, pageSize: Int

    init(client: ConsoleClient, pageSize: Int = 50) { self.client = client; self.pageSize = pageSize }

    var isLoading: Bool { board.loading || tree.loading }

    /// Reads the first page, and the tree when it is showing. Pages beyond the first are read again on demand.
    func refresh() async {
        await board.load { [client, includeFinished, pageSize] in
            let page = try await client.board(all: includeFinished, limit: pageSize)
            return RunsPage(lanes: page.lanes, nextCursor: page.nextCursor, coverage: page.coverage, observedAt: page.observedAt)
        }
        moreError = nil
        if mode == .tree { await loadTree() }
    }

    func loadTree() async { await tree.load { [client] in try await client.tree() } }

    /// Reads the page after the last one. A cursor from a changed board is refused by the CLI; the error says to refresh.
    func loadMore() async {
        guard !loadingMore, var page = board.value, let cursor = page.nextCursor else { return }
        loadingMore = true
        defer { loadingMore = false }
        do {
            let next = try await client.board(all: includeFinished, limit: pageSize, cursor: cursor)
            let known = Set(page.lanes.map(\.id))
            page.lanes += next.lanes.filter { !known.contains($0.id) }
            page.nextCursor = next.nextCursor
            page.coverage = next.coverage
            board.replace(page)
            moreError = nil
        } catch {
            moreError = Reader<RunsPage>.describe(error)
        }
    }

    func setMode(_ mode: Mode) async {
        self.mode = mode
        if mode == .tree, tree.isFirstLoad { await loadTree() }
    }

    /// The tree's nodes in reading order: roots first, each followed by its children, indented by depth.
    nonisolated static func ordered(_ nodes: [TreeNode]) -> [TreeNode] {
        let byID = Dictionary(nodes.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let childIDs = Set(nodes.flatMap { ($0.children ?? []).map(\.attemptId) }.filter { byID[$0] != nil })
        var seen = Set<String>(), result: [TreeNode] = []
        func visit(_ node: TreeNode) {
            guard seen.insert(node.id).inserted else { return }
            result.append(node)
            for child in node.children ?? [] { if let next = byID[child.attemptId] { visit(next) } }
        }
        nodes.filter { !childIDs.contains($0.id) }.forEach(visit)
        nodes.forEach(visit)   // anything in a cycle or orphaned by the node limit
        return result
    }
}
