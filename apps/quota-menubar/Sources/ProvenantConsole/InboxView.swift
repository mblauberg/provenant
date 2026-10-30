import ProvenantKit
import SwiftUI

extension HumanRequest {
    var kindTitle: String { request.kind.capitalized }
    /// The chair session that asked, shortened for the list.
    var sessionTitle: String { String(chairSessionId.prefix(8)) }
    /// "Project · " before the session, or nothing when the project is unknown.
    var projectPrefix: String { projectName.map { "\($0) · " } ?? "" }
}

@available(macOS 26, *)
struct InboxView: View {
    @Bindable var model: InboxModel

    var body: some View {
        NavigationSplitView {
            List(model.requests, selection: $model.selection) { request in
                RequestRow(request: request).tag(request.id)
            }
            .accessibilityLabel("Pending requests")
            .overlay { if model.requests.isEmpty { emptyState } }
            .navigationSplitViewColumnWidth(min: 240, ideal: 300)
        } detail: {
            if let request = model.selected {
                DetailView(model: model, request: request).id("\(request.id)@\(request.revision)")
            } else {
                Text("Select a request").foregroundStyle(.secondary)
            }
        }
        .toolbar {
            ToolbarItem {
                Button { Task { await model.refresh() } } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(model.refreshing)
                    .accessibilityLabel("Refresh inbox")
            }
        }
        .safeAreaInset(edge: .bottom) { footer }
        .frame(minWidth: 720, minHeight: 420)
    }

    private var emptyState: some View {
        VStack(spacing: 6) {
            Text(model.lastRefreshed == nil && model.refreshError == nil ? "Reading the inbox…" : "Nothing needs you")
                .font(.headline)
            if let error = model.refreshError { Text(error).font(.callout).foregroundStyle(.secondary) }
        }.padding()
    }

    private var footer: some View {
        HStack {
            if let error = model.refreshError {
                Label("Refresh failed: \(error)", systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
            } else if let at = model.lastRefreshed {
                Text("Updated \(at.formatted(date: .omitted, time: .standard))").foregroundStyle(.secondary)
            }
            if model.truncated { Text("More requests exist than are shown.").foregroundStyle(.secondary) }
            Spacer()
        }
        .font(.caption).padding(.horizontal, 12).padding(.vertical, 6)
        .accessibilityElement(children: .combine)
    }
}

@available(macOS 26, *)
private struct RequestRow: View {
    let request: HumanRequest
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(request.request.title).lineLimit(2)
            HStack(spacing: 6) {
                Text(request.kindTitle)
                if request.request.blocking { Text("Blocking").foregroundStyle(.orange) }
                Text("\(request.projectPrefix)Session \(request.sessionTitle)")
            }.font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(request.kindTitle), \(request.request.blocking ? "blocking" : "not blocking"), \(request.projectName.map { "project \($0), " } ?? "")session \(request.sessionTitle): \(request.request.title)")
    }
}

@available(macOS 26, *)
private struct DetailView: View {
    let model: InboxModel
    let request: HumanRequest
    @State private var option: String?
    @State private var reply = ""
    @FocusState private var replyFocused: Bool

    private var result: InboxModel.Result? { model.results[request.id] }
    private var item: InboxItem? { result?.item }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(request.request.title).font(.title2).accessibilityAddTraits(.isHeader)
                Text("\(request.kindTitle) · \(request.request.blocking ? "Blocking" : "Not blocking") · \(request.projectPrefix)Session \(request.chairSessionId) · Revision \(request.revision) · \(request.state)")
                    .font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
                Text(request.request.body).textSelection(.enabled)
                if let proceeding = request.request.proceedingWith {
                    Text("Proceeding with: \(proceeding)").foregroundStyle(.secondary)
                }
                if let result { ResultBanner(result: result) }
                if let answer = item?.answer { AnswerSummary(answer: answer, delivery: item?.delivery) }
                // A saved answer closes the request even when the window could not read the item back.
                if request.isOpen, result?.kind != .saved { form }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder private var form: some View {
        Divider()
        if !request.request.options.isEmpty {
            Picker("Option", selection: $option) {
                ForEach(request.request.options) { choice in
                    VStack(alignment: .leading) {
                        Text(choice.label + (choice.id == request.request.recommendedOptionId ? " (recommended)" : ""))
                        Text(choice.description).font(.caption).foregroundStyle(.secondary)
                    }.tag(Optional(choice.id))
                }
            }
            .pickerStyle(.radioGroup)
            .accessibilityLabel("Answer options")
            .onAppear { option = option ?? request.request.recommendedOptionId }
        }
        TextField("Reply (optional)", text: $reply, axis: .vertical)
            .lineLimit(1...6)
            .focused($replyFocused)
            .accessibilityLabel("Reply text")
        HStack {
            if request.request.kind == "assumption" || request.request.proceedingWith != nil {
                Button("Confirm") { send(.confirm) }.accessibilityLabel("Confirm the assumption")
                Button("Override") { send(.override) }.disabled(trimmed.isEmpty).accessibilityLabel("Override with the reply text")
            }
            if !request.request.options.isEmpty {
                Button("Send answer") { send(.choose) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(option == nil)
                    .accessibilityLabel("Send the chosen option")
            }
            Button("Send reply") { send(.reply) }
                .keyboardShortcut(request.request.options.isEmpty ? .defaultAction : nil)
                .disabled(trimmed.isEmpty)
                .accessibilityLabel("Send the reply text")
            if model.sending { ProgressView().controlSize(.small) }
        }
        .disabled(model.sending)
    }

    private var trimmed: String { reply.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func send(_ action: AnswerAction) {
        let text = reply.isEmpty ? nil : reply
        let chosen = action == .choose ? option : nil
        Task { await model.answer(request, action: action, option: chosen, text: text) }
    }
}

@available(macOS 26, *)
private struct ResultBanner: View {
    let result: InboxModel.Result
    var body: some View {
        Label(result.message, systemImage: result.kind == .saved ? "checkmark.circle" : "exclamationmark.triangle")
            .foregroundStyle(result.kind == .saved ? Color.green : Color.orange)
            .accessibilityElement(children: .combine)
    }
}

@available(macOS 26, *)
private struct AnswerSummary: View {
    let answer: HumanAnswer
    let delivery: AnswerDelivery?
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Answer").font(.headline)
            Text("\(answer.action.capitalized) via \(answer.channel) at \(answer.answeredAt)")
            if let id = answer.optionId { Text("Option: \(id)") }
            if let text = answer.text { Text(text).textSelection(.enabled) }
            Text(delivery.map(InboxModel.deliveryLine) ?? "No delivery recorded.").foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}
