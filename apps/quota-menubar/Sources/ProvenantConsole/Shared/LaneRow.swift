import ProvenantDesign
import ProvenantKit
import SwiftUI

/// One attempt in a list: its title, state and, when known, route and what it waits for.
struct LaneRow: View {
    let title: String
    let state: String
    let outcome: String?
    let route: Route?
    let waiting: String?
    let attention: Bool
    /// The project the attempt belongs to, shown beside its state; not what it waits on.
    var project: String? = nil
    var depth = 0

    var style: StatusStyle { .attempt(state: state, outcome: outcome) }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.small) {
            Image(systemName: style.symbol)
                .foregroundStyle(style.tone.color)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).lineLimit(2)
                Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: Space.small)
            if attention {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    .help("Needs attention")
                    .accessibilityLabel("Needs attention")
            }
        }
        .padding(.leading, CGFloat(depth) * 18)
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title), \(style.label)\(attention ? ", needs attention" : "")\(waiting.map { ", " + $0 } ?? "")")
    }

    private var subtitle: String {
        [style.label, project, route?.summary, waiting].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

extension LaneRow {
    init(_ lane: BoardLane, depth: Int = 0) {
        let state = lane.metadata.attempt.state
        self.init(title: lane.title, state: state.state, outcome: state.outcome, route: lane.metadata.attempt.requestedRoute,
                  waiting: lane.wait?.waitingFor, attention: lane.attention, depth: depth)
    }

    init(_ lane: OverviewLane) {
        self.init(title: lane.title, state: lane.state, outcome: lane.outcome, route: nil,
                  waiting: nil, attention: lane.attention, project: lane.projectLabel)
    }

    init(_ node: TreeNode) {
        let state = node.metadata.attempt.state
        self.init(title: node.metadata.attempt.title ?? "Attempt \(node.id.prefix(8))", state: state.state, outcome: state.outcome,
                  route: node.metadata.attempt.requestedRoute, waiting: nil, attention: false, depth: node.depth)
    }
}
