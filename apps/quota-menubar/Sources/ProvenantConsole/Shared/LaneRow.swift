import ProvenantDesign
import ProvenantKit
import SwiftUI

/// One attempt in a list: its title, state and, when known, route and what it waits for.
struct LaneRow: View {
    let title: String
    let state: AttemptState
    let route: Route?
    let waiting: String?
    let attention: Bool
    var depth = 0

    private var style: StatusStyle { .attempt(state: state.state, outcome: state.outcome) }

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
        [style.label, route?.summary, waiting].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

extension LaneRow {
    init(_ lane: BoardLane, depth: Int = 0) {
        self.init(title: lane.title, state: lane.metadata.attempt.state, route: lane.metadata.attempt.requestedRoute,
                  waiting: lane.wait?.waitingFor, attention: lane.attention, depth: depth)
    }

    init(_ node: TreeNode) {
        self.init(title: node.metadata.attempt.title ?? "Attempt \(node.id.prefix(8))", state: node.metadata.attempt.state,
                  route: node.metadata.attempt.requestedRoute, waiting: nil, attention: false, depth: node.depth)
    }
}
