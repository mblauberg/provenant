import ProvenantDesign
import ProvenantKit
import SwiftUI

struct RunsView: View {
    let console: ConsoleModel
    @Bindable var runs: RunsModel

    init(console: ConsoleModel) { self.console = console; runs = console.runs }

    var body: some View {
        Group {
            if runs.mode == .tree { treeBody } else { listBody }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { footer }
        .onChange(of: runs.selection) { _, id in if let id { console.showAttempt(id) } }
    }

    // MARK: List

    @ViewBuilder private var listBody: some View {
        if let page = runs.board.value {
            List(selection: $runs.selection) {
                if page.coverage.runs != "complete" && page.coverage.runs != "empty" {
                    Notice("Run coverage: \(page.coverage.runs.replacingOccurrences(of: "_", with: " ")).", tone: .neutral)
                }
                if let error = runs.board.error { Notice("Refresh failed. \(error)") }
                ForEach(page.lanes) { lane in LaneRow(lane).tag(lane.id) }
                if page.nextCursor != nil {
                    HStack {
                        Button("Load More") { Task { await runs.loadMore() } }.disabled(runs.loadingMore)
                        if runs.loadingMore { ProgressView().controlSize(.small) }
                        if let error = runs.moreError { Text(error).font(.caption).foregroundStyle(.secondary) }
                    }
                }
            }
            .listStyle(.inset)
            .overlay {
                if page.lanes.isEmpty {
                    EmptyPanel(runs.includeFinished ? "No Runs" : "Nothing Running", symbol: "bolt.horizontal",
                               message: runs.includeFinished ? "No attempts are recorded on this host."
                                                              : "Turn on Show Finished to include retained attempts.")
                }
            }
        } else if let error = runs.board.error {
            ErrorPanel("Can’t Read the Runs", message: error) { Task { await runs.refresh() } }
        } else {
            LoadingPanel("Reading the runs…")
        }
    }

    // MARK: Tree

    @ViewBuilder private var treeBody: some View {
        if let tree = runs.tree.value {
            let nodes = RunsModel.ordered(tree.nodes)
            List(selection: $runs.selection) {
                if tree.truncated { Notice("The tree is limited to the first \(tree.nodes.count) attempts.", tone: .neutral) }
                ForEach(nodes) { node in LaneRow(node).tag(node.id) }
            }
            .listStyle(.inset)
            .overlay {
                if nodes.isEmpty { EmptyPanel("No Nested Runs", symbol: "list.bullet.indent", message: "No attempts are recorded on this host.") }
            }
        } else if let error = runs.tree.error {
            ErrorPanel("Can’t Read the Run Tree", message: error) { Task { await runs.loadTree() } }
        } else {
            LoadingPanel("Reading the run tree…")
        }
    }

    private var footer: some View {
        HStack(spacing: Space.medium) {
            Toggle("Show Finished", isOn: $runs.includeFinished)
                .toggleStyle(.checkbox)
                .disabled(runs.mode == .tree)
            Spacer(minLength: Space.small)
            if runs.isLoading { ProgressView().controlSize(.mini).accessibilityLabel("Refreshing") }
            else if let at = runs.board.loadedAt { Text("Updated \(Stamp.relative(at, now: console.inbox.displayNow))") }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, Space.medium)
        .padding(.vertical, 5)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}
