import ProvenantDesign
import ProvenantKit
import SwiftUI

struct RequestRow: View {
    let request: HumanRequest
    let now: Date

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: request.kindSymbol)
                .font(.body.weight(.semibold))
                .foregroundStyle(request.kindTint)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: Space.small) {
                    Text(request.request.title)
                        .font(.callout)
                        .lineLimit(2)
                        .layoutPriority(1)
                    Spacer(minLength: Space.tight)
                    Text(request.ageText(now: now))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                }
                HStack(spacing: 6) {
                    if request.request.blocking {
                        Label("Blocking", systemImage: "exclamationmark.octagon.fill")
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(.orange)
                            .help("Blocks the session until answered")
                    }
                    Text(request.projectPrefix + "Session \(request.sessionTitle)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .padding(.vertical, Space.tight)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private var accessibilityLabel: String {
        var parts = [request.kindTitle]
        if request.request.blocking { parts.append("blocking") }
        if let project = request.projectName { parts.append("project \(project)") }
        parts.append("session \(request.sessionTitle)")
        parts.append(request.request.title)
        parts.append("created \(Stamp.relative(request.createdAt, now: now))")
        return parts.joined(separator: ", ")
    }
}
