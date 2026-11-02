import ProvenantDesign
import ProvenantKit
import SwiftUI

/// What the last answer for a request did: saved, found stale, or failed.
struct ResultBanner: View {
    let result: InboxModel.Result
    private var tint: Color { result.kind == .saved ? .green : .orange }

    var body: some View {
        HStack(alignment: .top, spacing: Space.small) {
            Image(systemName: result.kind == .saved ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            Text(result.message).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(tint.opacity(0.10), in: RoundedRectangle(cornerRadius: Space.small))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(result.message)
    }
}

/// The recorded answer and its delivery, from the item the CLI returned.
struct AnswerSummary: View {
    let answer: HumanAnswer
    let delivery: AnswerDelivery?
    let now: Date

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 5) {
                Text("\(answer.action.capitalized) via \(answer.channel), \(Stamp.relative(answer.answeredAt, now: now))")
                    .help(Stamp.absolute(answer.answeredAt))
                if let id = answer.optionId { Text("Option: \(id)") }
                if let text = answer.text { Text(text).textSelection(.enabled) }
                Text(delivery.map { InboxModel.deliveryLine($0, now: now) } ?? "No delivery recorded.")
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        } label: {
            Label("Answer", systemImage: "checkmark.circle")
        }
    }
}
