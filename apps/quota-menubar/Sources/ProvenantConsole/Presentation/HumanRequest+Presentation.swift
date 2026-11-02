import ProvenantDesign
import ProvenantKit
import SwiftUI

extension HumanRequest {
    var kindTitle: String { kind.title }
    /// The chair session that asked, shortened for the list.
    var sessionTitle: String { String(chairSessionId.prefix(8)) }
    /// "Project · " before the session, or nothing when the project is unknown.
    var projectPrefix: String { projectDisplayName.map { "\($0) · " } ?? "" }
    /// The project's name, with the host it lives on when a peer owns it: "api on studio".
    var projectDisplayName: String? {
        guard let peerLabel else { return projectName }
        return "\(projectName ?? "Project") on \(peerLabel)"
    }
    var createdAtDate: Date? { Stamp.parse(createdAt) }

    var kindSymbol: String {
        switch kind {
        case .decision: "arrow.triangle.branch"
        case .question: "questionmark.circle"
        case .assumption: "lightbulb"
        case .other: "bubble.left"
        }
    }

    var kindTint: Color {
        switch kind {
        case .decision: .blue
        case .question: .purple
        case .assumption: .teal
        case .other: .secondary
        }
    }

    /// A compact age for a dense row: now, 4m, 2h, 3d, then the date.
    func ageText(now: Date) -> String {
        guard let date = createdAtDate else { return "" }
        let seconds = max(0, now.timeIntervalSince(date))
        switch seconds {
        case ..<60: return "now"
        case ..<3_600: return "\(Int(seconds / 60))m"
        case ..<86_400: return "\(Int(seconds / 3_600))h"
        case ..<604_800: return "\(Int(seconds / 86_400))d"
        default: return date.formatted(.dateTime.month(.abbreviated).day())
        }
    }
}
