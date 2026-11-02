import ProvenantDesign
import ProvenantKit
import SwiftUI

/// The Inbox screen: the open requests beside the selected one. Search is the window's search field.
struct InboxScreen: View {
    @Bindable var model: InboxModel

    var body: some View {
        HSplitView {
            RequestList(model: model)
                .frame(minWidth: 300, idealWidth: 360, maxWidth: 520)
            detail.frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
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

    @ViewBuilder private var detail: some View {
        if let request = model.selected {
            RequestDetailView(model: model, request: request).id("\(request.id)@\(request.revision)")
        } else {
            EmptyPanel("No Request Selected", symbol: "sidebar.left", message: "Choose a request to read it and answer.")
        }
    }
}

struct RequestList: View {
    @Bindable var model: InboxModel
    @FocusState private var listFocused: Bool

    var body: some View {
        List(model.visibleRequests, selection: $model.selection) { request in
            RequestRow(request: request, now: model.displayNow)
                .tag(request.id)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 7, leading: Space.medium, bottom: 7, trailing: Space.medium))
        }
        .listStyle(.inset)
        .accessibilityLabel("Open requests")
        .focused($listFocused)
        .onAppear { listFocused = true }
        .safeAreaInset(edge: .top, spacing: 0) {
            if let error = model.refreshError, !model.requests.isEmpty {
                VStack(spacing: 0) {
                    Notice("Refresh failed. \(error)")
                    Divider()
                }
                .background(.bar)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { statusBar }
        .overlay { overlay }
    }

    @ViewBuilder private var overlay: some View {
        if model.requests.isEmpty {
            if model.refreshing && model.lastRefreshed == nil {
                LoadingPanel("Reading the inbox…")
            } else if let error = model.refreshError {
                ErrorPanel("Can’t Reach the Inbox", message: error) { Task { await model.refresh() } }
            } else {
                EmptyPanel("No open requests", symbol: "tray",
                           message: "Requests that need a decision, an answer or an assumption confirmed appear here. The inbox refreshes when the app comes to the front, when something changes, and with ⌘R.")
            }
        } else if model.visibleRequests.isEmpty {
            noMatches
        }
    }

    private var noMatches: some View {
        ContentUnavailableView {
            Label("No Matching Requests", systemImage: "magnifyingglass")
        } description: {
            Text(model.query.isEmpty
                 ? "This view has no open requests. Choose another filter or project."
                 : "Nothing in \(scopeTitle) matches “\(model.query)”.")
        } actions: {
            if !model.query.isEmpty { Button("Clear Search") { model.searchText = "" } }
            Button("Show All Requests") {
                model.searchText = ""
                model.show(.all)
            }
        }
    }

    private var scopeTitle: String {
        switch model.scope {
        case .filter(let filter): filter.title
        case .project(let id): model.projectGroups.first { $0.id == id }?.name ?? "this project"
        }
    }

    private var statusBar: some View {
        HStack(spacing: 10) {
            if model.refreshing {
                ProgressView().controlSize(.mini).accessibilityLabel("Refreshing")
            } else if let at = model.lastRefreshed {
                Text("Updated \(Stamp.relative(at, now: model.displayNow))")
            } else {
                Text("Not yet read")
            }
            if model.truncated {
                Text("Older requests were not loaded.")
                    .help("Search and projects cover only the requests shown. Run `provenant inbox --all-projects` to see them all.")
            }
            Spacer(minLength: Space.small)
            Text("j/k move · 1-9 choose")
                .accessibilityLabel("Keyboard: j and k move, number keys choose an option")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, Space.medium)
        .padding(.vertical, 5)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
        .accessibilityElement(children: .combine)
    }
}
