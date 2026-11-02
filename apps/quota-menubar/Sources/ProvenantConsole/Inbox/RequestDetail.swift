import ProvenantDesign
import ProvenantKit
import SwiftUI

struct RequestDetailView: View {
    let model: InboxModel
    let request: HumanRequest

    private var result: InboxModel.Result? { model.results[request.id] }
    private var item: InboxItem? { result?.item }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header
                if !request.request.body.isEmpty {
                    Text(markdownBody)
                        .font(.body)
                        .lineSpacing(3)
                        .textSelection(.enabled)
                }
                if let proceeding = request.request.proceedingWith {
                    GroupBox {
                        Text(proceeding).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    } label: {
                        Label("Proceeding with", systemImage: "arrow.turn.down.right")
                    }
                }
                if let since = model.staleSince(for: request) {
                    Notice("\(request.peerLabel ?? "Its host") is not answering. This request is from the last read, \(Stamp.relative(since, now: model.displayNow)). You can still answer; the answer is sent only if the host can be reached.")
                        .background(.quaternary, in: .rect(cornerRadius: 8))
                }
                if let result { ResultBanner(result: result) }
                if let answer = item?.answer { AnswerSummary(answer: answer, delivery: item?.delivery, now: model.displayNow) }
                // A saved answer closes the request even when the window could not read the item back.
                if request.isOpen, result?.kind != .saved { AnswerForm(model: model, request: request) }
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 24)
            .frame(maxWidth: Space.readingWidth, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Space.small) {
            HStack(spacing: Space.small) {
                Label(request.kindTitle, systemImage: request.kindSymbol)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(request.kindTint)
                if request.request.blocking {
                    Label("Blocking", systemImage: "exclamationmark.octagon.fill")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.orange)
                        .help("Blocks the session until answered")
                }
                Spacer(minLength: Space.small)
                Text(Stamp.relative(request.createdAt, now: model.displayNow))
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .help(Stamp.absolute(request.createdAt))
            }
            Text(request.request.title)
                .font(.title2.weight(.semibold))
                .textSelection(.enabled)
                .accessibilityAddTraits(.isHeader)
            Text(metadataLine)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
    }

    private var metadataLine: String {
        [request.projectDisplayName ?? "No project", "Session \(request.sessionTitle)", "Revision \(request.revision)", request.state.capitalized]
            .joined(separator: " · ")
    }

    private var markdownBody: AttributedString {
        let parsed = try? AttributedString(markdown: request.request.body, options: .init(interpretedSyntax: .full))
        return parsed ?? AttributedString(request.request.body)
    }
}
