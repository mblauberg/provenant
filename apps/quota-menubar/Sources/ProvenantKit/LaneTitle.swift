import Foundation

/// Names a lane for a person supervising work. The CLI gives each attempt a
/// `lane_id.key` of `{graph, node, attempt}`; the node is `task` for a graph
/// that has only its default node, so it is not a useful title on its own.
public enum LaneTitle {
    /// The node a single-node graph runs at. It never distinguishes that lane.
    public static let defaultNode = "task"

    /// The graph name, with the node only when it is not the default, and the
    /// attempt only when it is a retry. Nil when the key holds no name at all.
    public static func title(graph: String?, node: String?, attempt: UInt64?) -> String? {
        let graph = graph?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        let node = node?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        let meaningfulNode = node.flatMap { $0 == defaultNode ? nil : $0 }
        guard let lead = graph ?? meaningfulNode else { return nil }

        var parts = [lead]
        if let meaningfulNode, meaningfulNode != lead { parts.append(meaningfulNode) }
        if let attempt, attempt > 1 { parts.append("attempt \(attempt)") }
        return parts.joined(separator: " · ")
    }

    public static func title(_ key: LaneID.Key?) -> String? {
        guard let key else { return nil }
        return title(graph: key.graph, node: key.node, attempt: key.attempt)
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
