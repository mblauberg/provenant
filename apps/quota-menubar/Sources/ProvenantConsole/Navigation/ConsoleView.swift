import ProvenantDesign
import ProvenantKit
import SwiftUI

/// The window: sidebar, the chosen screen, and the attempt inspector. Search, refresh and the inspector are the
/// toolbar; every one of them is also a menu command (see App.swift).
struct ConsoleView: View {
    @Bindable var console: ConsoleModel

    var body: some View {
        NavigationSplitView {
            SidebarView(console: console)
                .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 300)
        } detail: {
            screen
                .navigationTitle(console.destination.title)
        }
        .searchable(text: $console.searchText, isPresented: $console.searching, prompt: "Search")
        .searchScopes($console.searchScope) {
            ForEach(SearchScope.allCases) { Text($0.title).tag($0) }
        }
        .onSubmit(of: .search) { console.submitSearch() }
        .inspector(isPresented: $console.inspectorShown) {
            AttemptInspector(console: console)
                .inspectorColumnWidth(min: 280, ideal: 360, max: 520)
        }
        .toolbar {
            ToolbarItem {
                Button { Task { await console.refresh() } } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    .disabled(console.refreshing)
                    .help("Refresh (⌘R)")
            }
            if console.destination == .runs {
                ToolbarItem {
                    Picker("View", selection: Binding(get: { console.runs.mode }, set: { mode in Task { await console.runs.setMode(mode) } })) {
                        ForEach(RunsModel.Mode.allCases) { Label($0.title, systemImage: $0.symbol).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .help("Show runs as a list or a tree")
                }
            }
            ToolbarItem {
                Button { console.inspectorShown.toggle() } label: { Label("Inspector", systemImage: "sidebar.trailing") }
                    .help("Show or hide the attempt inspector (⌥⌘I)")
            }
        }
        .frame(minWidth: 960, minHeight: 560)
    }

    @ViewBuilder private var screen: some View {
        switch console.destination {
        case .overview: OverviewView(console: console)
        case .inbox: InboxScreen(model: console.inbox)
        case .runs: RunsView(console: console)
        case .quota: QuotaView(model: console.quota)
        case .chairs: ChairsView(model: console.chairs, now: console.inbox.displayNow)
        case .search: SearchView(console: console)
        }
    }
}

