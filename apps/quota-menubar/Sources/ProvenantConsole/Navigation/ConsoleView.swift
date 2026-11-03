import ProvenantDesign
import ProvenantKit
import SwiftUI

/// The window: sidebar, the chosen screen, and the attempt inspector. Search, refresh and the inspector are the
/// toolbar; every one of them is also a menu command (see App.swift).
struct ConsoleView: View {
    @Bindable var console: ConsoleModel

    @State private var initialInboxWidth: Double?

    static let sidebarMinimum: CGFloat = 190
    static let inboxMinimum: CGFloat = 220
    static let detailMinimum: CGFloat = 270
    static let inspectorMinimum: CGFloat = 280
    static let windowMinimum: CGFloat = 960
    private var inboxMaximum: CGFloat { max(InboxColumnWidth.maximum(), Self.inboxMinimum) }

    var body: some View {
        columns
        .onAppear { initialInboxWidth = InboxColumnWidth.load() }
        .onChange(of: console.destination) { _, destination in
            if destination == .inbox { initialInboxWidth = InboxColumnWidth.load() }
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "jkJK")) { press in
            guard console.destination == .inbox, !console.inbox.isTypingReply, !console.inbox.searching, let character = press.characters.first else { return .ignored }
            if character == "j" || character == "J" { console.inbox.selectNext() } else { console.inbox.selectPrevious() }
            return .handled
        }
        .onKeyPress(characters: .decimalDigits) { press in
            guard console.destination == .inbox, !console.inbox.isTypingReply, !console.inbox.searching, let request = console.inbox.selected,
                  let number = press.characters.first?.wholeNumberValue,
                  console.inbox.chooseOption(number: number, for: request) else { return .ignored }
            return .handled
        }

        .searchable(text: $console.searchText, isPresented: $console.searching, prompt: "Search")
        .searchScopes($console.searchScope) {
            ForEach(SearchScope.allCases) { Text($0.title).tag($0) }
        }
        .onSubmit(of: .search) { console.submitSearch() }
        .inspector(isPresented: $console.inspectorShown) {
            AttemptInspector(console: console)
                .inspectorColumnWidth(min: Self.inspectorMinimum, ideal: 360, max: 520)
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
        .frame(minWidth: Self.windowMinimum, minHeight: 560)
    }

    @ViewBuilder private var columns: some View {
        if console.destination == .inbox, let initialInboxWidth {
            NavigationSplitView {
                sidebar
            } content: {
                RequestList(model: console.inbox)
                    .navigationSplitViewColumnWidth(min: Self.inboxMinimum, ideal: InboxColumnWidth.clamped(initialInboxWidth, minimum: Self.inboxMinimum, maximum: inboxMaximum), max: inboxMaximum)
                    .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width in
                        if width >= Self.inboxMinimum { InboxColumnWidth.save(width) }
                    }
            } detail: {
                InboxScreen(model: console.inbox)
                    .frame(minWidth: Self.detailMinimum)
                    .navigationTitle(console.destination.title)
            }
        } else {
            NavigationSplitView {
                sidebar
            } detail: {
                screen.navigationTitle(console.destination.title)
            }
        }
    }

    private var sidebar: some View {
        SidebarView(console: console)
            .navigationSplitViewColumnWidth(min: Self.sidebarMinimum, ideal: 220, max: 300)
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


/// A remembered width is an initial preference; the native divider remains free to resize.
/// The Inbox list column's remembered width. The maximum is a default the
/// user can override with `defaults write <bundle> inboxColumnMaximum <points>`.
enum InboxColumnWidth {
    private static let key = "inboxColumnWidth"
    private static let maximumKey = "inboxColumnMaximum"
    static let defaultMaximum: Double = 720
    static func maximum(from defaults: UserDefaults = .standard) -> Double {
        defaults.object(forKey: maximumKey) as? Double ?? defaultMaximum
    }
    static func clamped(_ width: Double, minimum: Double, maximum: Double) -> Double {
        min(max(width, minimum), max(maximum, minimum))
    }
    static func load(from defaults: UserDefaults = .standard) -> Double {
        defaults.object(forKey: key) as? Double ?? 300
    }
    static func save(_ width: Double, to defaults: UserDefaults = .standard) {
        defaults.set(width, forKey: key)
    }
}
