import ProvenantKit
import SwiftUI

extension HumanRequest {
    var kindTitle: String { request.kind.capitalized }
    /// The chair session that asked, shortened for the list.
    var sessionTitle: String { String(chairSessionId.prefix(8)) }
    /// "Project · " before the session, or nothing when the project is unknown.
    var projectPrefix: String { projectName.map { "\($0) · " } ?? "" }

    var kindSymbol: String {
        switch request.kind.lowercased() {
        case "decision": "arrow.triangle.branch"
        case "question": "questionmark.circle"
        case "assumption": "lightbulb"
        default: "bubble.left"
        }
    }

    var kindTint: Color {
        switch request.kind.lowercased() {
        case "decision": .blue
        case "question": .purple
        case "assumption": .teal
        default: .secondary
        }
    }

    var createdAtDate: Date? {
        if let date = try? Date(createdAt, strategy: .iso8601) { return date }
        return try? Date(createdAt, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
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

@available(macOS 26, *)
struct InboxView: View {
    @Bindable var model: InboxModel
    @FocusState private var listFocused: Bool

    var body: some View {
        NavigationSplitView {
            sidebar
        } content: {
            requestList
                .navigationSplitViewColumnWidth(min: 300, ideal: 360, max: 520)
        } detail: {
            detail
        }
        .navigationTitle("Provenant Console")
        .safeAreaInset(edge: .bottom, spacing: 0) { statusBar }
        .frame(minWidth: 860, minHeight: 520)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { Task { await model.refresh() } } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .keyboardShortcut("r", modifiers: .command)
                .disabled(model.refreshing)
                .help("Refresh the inbox (⌘R)")
                .accessibilityLabel("Refresh inbox")
            }
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "jkJK")) { press in
            guard !model.isTypingReply, !model.searching, let character = press.characters.first else { return .ignored }
            if character == "j" || character == "J" { model.selectNext() } else { model.selectPrevious() }
            return .handled
        }
        .onKeyPress(characters: .decimalDigits) { press in
            guard !model.isTypingReply, !model.searching, let request = model.selected,
                  let number = press.characters.first?.wholeNumberValue,
                  model.chooseOption(number: number, for: request) else { return .ignored }
            return .handled
        }
    }

    private var sidebar: some View {
        List(selection: Binding<InboxModel.Scope?>(
            get: { model.scope },
            set: { if let scope = $0 { model.show(scope) } }
        )) {
            Section("Inbox") {
                filterRow(.all)
                filterRow(.blocking)
            }
            Section("Kinds") {
                filterRow(.decisions)
                filterRow(.questions)
                filterRow(.assumptions)
            }
            if !model.projectGroups.isEmpty {
                Section("Projects") {
                    ForEach(model.projectGroups) { project in
                        Label(project.name, systemImage: "folder")
                            .badge(project.count)
                            .tag(InboxModel.Scope.project(project.id))
                            .accessibilityLabel("\(project.name), \(project.count) \(project.count == 1 ? "open request" : "open requests")")
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 300)
        .accessibilityLabel("Inbox filters and projects")
    }

    private func filterRow(_ filter: InboxModel.Filter) -> some View {
        Label(filter.title, systemImage: filter.symbol)
            .badge(model.count(for: filter))
            .tag(InboxModel.Scope.filter(filter))
            .accessibilityLabel("\(filter.title), \(model.count(for: filter)) \(model.count(for: filter) == 1 ? "open request" : "open requests")")
    }

    private var requestList: some View {
        List(model.visibleRequests, selection: $model.selection) { request in
            RequestRow(request: request, now: model.displayNow)
                .tag(request.id)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 7, leading: 12, bottom: 7, trailing: 12))
        }
        .listStyle(.inset)
        .accessibilityLabel("Open requests")
        .searchable(text: $model.searchText, isPresented: $model.searching, prompt: "Search requests")
        .focused($listFocused)
        .onAppear { listFocused = true }
        .safeAreaInset(edge: .top, spacing: 0) {
            if let error = model.refreshError, !model.requests.isEmpty { errorBanner(error) }
        }
        .overlay {
            if model.requests.isEmpty {
                if model.refreshing && model.lastRefreshed == nil {
                    ProgressView("Reading the inbox…").controlSize(.small)
                } else if let error = model.refreshError {
                    errorState(error)
                } else {
                    emptyState
                }
            } else if model.visibleRequests.isEmpty {
                noMatchesState
            }
        }
    }

    @ViewBuilder private var detail: some View {
        if let request = model.selected {
            RequestDetailView(model: model, request: request).id("\(request.id)@\(request.revision)")
        } else {
            ContentUnavailableView {
                Label("No Request Selected", systemImage: "sidebar.left")
            } description: {
                Text("Choose a request to read it and answer.")
            }
        }
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No open requests", systemImage: "tray")
        } description: {
            Text("Requests that need a decision, an answer or an assumption confirmed appear here. The inbox refreshes when the app comes to the front, and with ⌘R.")
        } actions: {
            Button("Refresh") { Task { await model.refresh() } }
                .accessibilityLabel("Refresh the inbox")
        }
    }

    private func errorState(_ message: String) -> some View {
        ContentUnavailableView {
            Label("Can’t Reach the Inbox", systemImage: "exclamationmark.triangle")
        } description: {
            Text(message)
        } actions: {
            Button("Try Again") { Task { await model.refresh() } }
                .accessibilityLabel("Try reading the inbox again")
        }
    }

    private var noMatchesState: some View {
        ContentUnavailableView {
            Label("No Matching Requests", systemImage: "magnifyingglass")
        } description: {
            Text(model.query.isEmpty
                 ? "This view has no open requests. Choose another filter or project."
                 : "Nothing in \(scopeTitle) matches “\(model.query)”.")
        } actions: {
            if !model.query.isEmpty {
                Button("Clear Search") { model.searchText = "" }
            }
            Button("Show All Requests") {
                model.searchText = ""
                model.show(.filter(.all))
            }
        }
    }

    private var scopeTitle: String {
        switch model.scope {
        case .filter(let filter): filter.title
        case .project(let id): model.projectGroups.first { $0.id == id }?.name ?? "this project"
        }
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text("Refresh failed").font(.callout.weight(.medium))
                Text(message).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Refresh failed. \(message)")
            Spacer(minLength: 8)
            Button("Try Again") { Task { await model.refresh() } }
                .controlSize(.small)
                .accessibilityLabel("Try reading the inbox again")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
        .overlay(alignment: .bottom) { Divider() }
    }

    private var statusBar: some View {
        HStack(spacing: 10) {
            if model.refreshing {
                ProgressView().controlSize(.mini).accessibilityLabel("Refreshing")
            } else if let at = model.lastRefreshed {
                Text(updatedText(at))
            } else {
                Text("Not yet read")
            }
            if model.truncated {
                Text("More requests exist than are shown. Narrow the list with search or a project.")
            }
            Spacer(minLength: 8)
            Text("j/k move · 1-9 choose · Return sends")
                .accessibilityLabel("Keyboard: j and k move, number keys choose an option, Return sends")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
        .accessibilityElement(children: .combine)
    }

    private func updatedText(_ date: Date) -> String {
        let seconds = max(0, model.displayNow.timeIntervalSince(date))
        switch seconds {
        case ..<5: return "Updated just now"
        case ..<60: return "Updated \(Int(seconds))s ago"
        case ..<3_600: return "Updated \(Int(seconds / 60))m ago"
        default: return "Updated \(date.formatted(date: .omitted, time: .shortened))"
        }
    }
}

@available(macOS 26, *)
private struct RequestRow: View {
    let request: HumanRequest
    let now: Date

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: request.kindSymbol)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(request.kindTint)
                .frame(width: 17, height: 17)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(request.request.title)
                        .font(.callout)
                        .lineLimit(2)
                        .layoutPriority(1)
                    Spacer(minLength: 4)
                    Text(request.ageText(now: now))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                }
                HStack(spacing: 6) {
                    if request.request.blocking {
                        Label("Blocking", systemImage: "exclamationmark.octagon.fill")
                            .labelStyle(.titleAndIcon)
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
        .padding(.vertical, 4)
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
        if let date = request.createdAtDate {
            parts.append("created \(date.formatted(date: .abbreviated, time: .shortened))")
        }
        return parts.joined(separator: ", ")
    }
}

@available(macOS 26, *)
private struct RequestDetailView: View {
    let model: InboxModel
    let request: HumanRequest
    @FocusState private var replyFocused: Bool

    private var result: InboxModel.Result? { model.results[request.id] }
    private var item: InboxItem? { result?.item }
    private var options: [HumanRequest.Option] { request.request.options }
    private var recommended: String? { request.request.recommendedOptionId }
    private var isAssumption: Bool { request.request.kind.lowercased() == "assumption" || request.request.proceedingWith != nil }
    private var reply: String { model.reply(for: request) }
    private var trimmed: String { reply.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var option: String? { model.draft(for: request).optionID }

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
                if let result { ResultBanner(result: result) }
                if let answer = item?.answer { AnswerSummary(answer: answer, delivery: item?.delivery) }
                // A saved answer closes the request even when the window could not read the item back.
                if request.isOpen, result?.kind != .saved { answerForm }
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 24)
            .frame(maxWidth: 720, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
        }
        .onChange(of: replyFocused) { _, focused in model.isTypingReply = focused }
        .onDisappear { model.isTypingReply = false }
        .toolbar { secondaryActions }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Label(request.kindTitle, systemImage: request.kindSymbol)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(request.kindTint)
                if request.request.blocking {
                    Label("Blocking", systemImage: "exclamationmark.octagon.fill")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.orange)
                        .help("Blocks the session until answered")
                }
                Spacer(minLength: 8)
                Text(request.ageText(now: model.displayNow))
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.tertiary)
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
        [request.projectName ?? "No project", "Session \(request.sessionTitle)", "Revision \(request.revision)", request.state.capitalized]
            .joined(separator: " · ")
    }

    private var markdownBody: AttributedString {
        let parsed = try? AttributedString(markdown: request.request.body, options: .init(interpretedSyntax: .full))
        return parsed ?? AttributedString(request.request.body)
    }

    private var answerForm: some View {
        VStack(alignment: .leading, spacing: 16) {
            Divider()
            if !options.isEmpty {
                VStack(alignment: .leading, spacing: 7) {
                    HStack(alignment: .firstTextBaseline) {
                        Text("Choose an option").font(.headline)
                        Spacer(minLength: 8)
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
                primaryAction
                if model.sending {
                    ProgressView().controlSize(.small).accessibilityLabel("Sending answer")
                }
                Spacer(minLength: 0)
            }
        }
        .disabled(model.sending)
    }

    @ViewBuilder private var primaryAction: some View {
        if !options.isEmpty {
            Button("Send Answer") { send(.choose) }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(option == nil || model.sending)
                .help("Send the chosen option (Return)")
                .accessibilityLabel("Send the chosen option")
        } else if isAssumption {
            Button("Confirm") { send(.confirm) }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(model.sending)
                .help("Confirm the assumption (Return)")
                .accessibilityLabel("Confirm the assumption")
        } else {
            Button("Send Reply") { send(.reply) }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(trimmed.isEmpty || model.sending)
                .help("Send the reply (Return)")
                .accessibilityLabel("Send the reply")
        }
    }

    @ToolbarContentBuilder private var secondaryActions: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            if isAssumption, request.isOpen, result?.kind != .saved {
                Button("Override") { send(.override) }
                    .disabled(trimmed.isEmpty || model.sending)
                    .help("Override the assumption with the reply text")
                    .accessibilityLabel("Override with the reply text")
            }
            if !options.isEmpty, !trimmed.isEmpty, request.isOpen, result?.kind != .saved {
                Button("Send Reply Only") { send(.reply) }
                    .disabled(model.sending)
                    .help("Send the reply without choosing an option")
                    .accessibilityLabel("Send the reply without an option")
            }
        }
    }

    private func send(_ action: AnswerAction) {
        let text = reply.isEmpty ? nil : reply
        let chosen = action == .choose ? option : nil
        Task { await model.answer(request, action: action, option: chosen, text: text) }
    }
}

@available(macOS 26, *)
private struct ResultBanner: View {
    let result: InboxModel.Result
    private var tint: Color { result.kind == .saved ? .green : .orange }

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: result.kind == .saved ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            Text(result.message).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(tint.opacity(0.10), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(result.message)
    }
}

@available(macOS 26, *)
private struct AnswerSummary: View {
    let answer: HumanAnswer
    let delivery: AnswerDelivery?

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 5) {
                Text("\(answer.action.capitalized) via \(answer.channel) at \(answer.answeredAt)")
                if let id = answer.optionId { Text("Option: \(id)") }
                if let text = answer.text { Text(text).textSelection(.enabled) }
                Text(delivery.map(InboxModel.deliveryLine) ?? "No delivery recorded.")
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        } label: {
            Label("Answer", systemImage: "checkmark.circle")
        }
    }
}
