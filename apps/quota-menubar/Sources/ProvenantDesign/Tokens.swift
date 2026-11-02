import SwiftUI

/// Layout constants the console shares. Typography and colour are the system's: text styles scale with Dynamic Type
/// and colours are semantic, so nothing here names a font size or a fixed colour value.
public enum Space {
    public static let tight: CGFloat = 4
    public static let small: CGFloat = 8
    public static let medium: CGFloat = 12
    public static let large: CGFloat = 20
    /// The widest a reading column grows, so long text keeps a comfortable line length.
    public static let readingWidth: CGFloat = 720
}

/// The tone of a status: colour is used only to say how a thing is going, never to decorate.
public enum Tone: Sendable {
    case neutral, active, good, caution, bad

    public var color: Color {
        switch self {
        case .neutral: .secondary
        case .active: .blue
        case .good: .green
        case .caution: .orange
        case .bad: .red
        }
    }
}

/// How an attempt's lifecycle state reads: a label, a symbol and a tone. Unknown states from a newer CLI show
/// their raw name in the neutral tone.
public struct StatusStyle: Equatable, Sendable {
    public let label: String
    public let symbol: String
    public let tone: Tone

    public init(label: String, symbol: String, tone: Tone) { self.label = label; self.symbol = symbol; self.tone = tone }

    /// `state` is the lifecycle (`queued`…`terminal`); `outcome` refines a terminal state.
    public static func attempt(state: String, outcome: String? = nil) -> StatusStyle {
        switch state {
        case "queued": return StatusStyle(label: "Queued", symbol: "clock", tone: .neutral)
        case "starting": return StatusStyle(label: "Starting", symbol: "play.circle", tone: .active)
        case "running": return StatusStyle(label: "Running", symbol: "bolt.circle.fill", tone: .active)
        case "settling": return StatusStyle(label: "Settling", symbol: "hourglass", tone: .active)
        case "terminal":
            switch outcome {
            case "succeeded", nil: return StatusStyle(label: outcome == nil ? "Finished" : "Succeeded", symbol: "checkmark.circle.fill", tone: .good)
            case "cancelled", "canceled": return StatusStyle(label: "Cancelled", symbol: "minus.circle", tone: .neutral)
            case "task_failed": return StatusStyle(label: "Task failed", symbol: "xmark.circle.fill", tone: .bad)
            default: return StatusStyle(label: (outcome ?? "").replacingOccurrences(of: "_", with: " ").capitalized,
                                        symbol: "exclamationmark.circle.fill", tone: .bad)
            }
        default: return StatusStyle(label: state.replacingOccurrences(of: "_", with: " ").capitalized, symbol: "circle.dashed", tone: .neutral)
        }
    }
}
