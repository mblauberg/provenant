import ProvenantDesign
import ProvenantKit
import SwiftUI

/// Options, reply field and the buttons the request's kind allows. Return sends the primary action.
struct AnswerForm: View {
    let model: InboxModel
    let request: HumanRequest
    @FocusState private var replyFocused: Bool

    private var options: [HumanRequest.Option] { request.request.options }
    private var recommended: String? { request.request.recommendedOptionId }
    private var reply: String { model.reply(for: request) }
    private var trimmed: String { reply.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var option: String? { model.draft(for: request).optionID }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Divider()
            if !options.isEmpty { optionPicker }
            VStack(alignment: .leading, spacing: 7) {
                Text(options.isEmpty ? "Reply" : "Reply (optional)").font(.headline)
                TextField("Type a reply", text: Binding(
                    get: { model.reply(for: request) },
                    set: { model.setReply($0, for: request) }
                ), axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(2...6)
                .focused($replyFocused)
                .accessibilityLabel("Reply text")
            }
            HStack(spacing: 10) {
                actionButtons
                if model.sending {
                    ProgressView().controlSize(.small).accessibilityLabel("Sending answer")
                }
                Spacer(minLength: 0)
            }
        }
        .disabled(model.sending)
        .onChange(of: replyFocused) { _, focused in model.isTypingReply = focused }
        .onDisappear { model.isTypingReply = false }
    }

    private var optionPicker: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline) {
                Text("Choose an option").font(.headline)
                Spacer(minLength: Space.small)
                Text(options.count == 1 ? "Press 1" : "Press 1-\(min(options.count, 9))")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            Picker("Answer", selection: Binding(
                get: { model.draft(for: request).optionID },
                set: { model.setOption($0, for: request) }
            )) {
                ForEach(Array(options.enumerated()), id: \.element.id) { index, choice in
                    Text("\(index + 1). \(choice.label)\(choice.id == recommended ? " (Recommended)" : "")")
                        .tag(Optional(choice.id))
                }
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            .accessibilityLabel("Answer options")
            if let chosen = options.first(where: { $0.id == option }) {
                Text(chosen.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
    }

    /// Every action the request's kind allows, as on the CLI: choose an option, confirm or override an assumption,
    /// and reply. Return sends the first of choose, confirm, reply that applies.
    @ViewBuilder private var actionButtons: some View {
        let actions = InboxActions.available(for: request.request)
        let primary = InboxActions.primary(for: request.request)
        if actions.contains(.choose) {
            actionButton("Send Answer", .choose, primary: primary, disabled: option == nil,
                         help: "Send the chosen option", label: "Send the chosen option")
        }
        if actions.contains(.confirm) {
            actionButton("Confirm", .confirm, primary: primary, disabled: false,
                         help: "Confirm the assumption", label: "Confirm the assumption")
        }
        if actions.contains(.override) {
            actionButton("Override", .override, primary: primary, disabled: trimmed.isEmpty,
                         help: "Override the assumption with the reply text", label: "Override with the reply text")
        }
        if actions.contains(.reply) {
            actionButton("Send Reply", .reply, primary: primary, disabled: trimmed.isEmpty,
                         help: "Send the reply text", label: "Send the reply text")
        }
    }

    @ViewBuilder private func actionButton(_ title: String, _ action: AnswerAction, primary: AnswerAction,
                                           disabled: Bool, help: String, label: String) -> some View {
        if action == primary {
            Button(title) { send(action) }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(disabled || model.sending)
                .help("\(help) (Return)")
                .accessibilityLabel(label)
        } else {
            Button(title) { send(action) }
                .disabled(disabled || model.sending)
                .help(help)
                .accessibilityLabel(label)
        }
    }

    private func send(_ action: AnswerAction) {
        let text = reply.isEmpty ? nil : reply
        let chosen = action == .choose ? option : nil
        Task { await model.answer(request, action: action, option: chosen, text: text) }
    }
}
