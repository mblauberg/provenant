import Foundation
import Observation
import ProvenantKit

/// The window's state: where the sidebar points, which screens' data is out of date, and the models behind each
/// screen. It holds no timer. Data is read when a screen is chosen, when the app is activated, on ⌘R, and when the
/// change watcher says an owner write moved something the screen shows.
@MainActor @Observable
final class ConsoleModel {
    /// The one CLI client the screens and the change watcher share, so the CLI is looked up once.
    let client: ConsoleClient
    let inbox: InboxModel
    let overview: OverviewModel
    let runs: RunsModel
    let quota: QuotaPanelModel
    let chairs: ChairsModel
    let search: SearchModel
    let attempt: AttemptModel

    private(set) var item: SidebarItem
    var inspectorShown = false
    /// Whether the Inbox row shows its filters and projects.
    var inboxExpanded = true
    var searchScope: SearchScope = .requests {
        didSet { if searchScope != oldValue { syncInboxSearch() } }
    }
    var searchText = "" {
        didSet { if searchText != oldValue { syncInboxSearch() } }
    }
    /// True while the system search field is open. Navigation keys stand aside for typing.
    var searching = false {
        didSet { inbox.searching = searching }
    }

    /// Destinations whose data is older than the last known change, or was never read.
    private(set) var stale = Set(Destination.allCases)
    private let defaults: UserDefaults?
    private static let defaultsKey = "ConsoleDestination"

    var destination: Destination { item.destination }

    init(cli: CLIClient = CLIClient(), now: @escaping () -> Date = { .now }, defaults: UserDefaults? = nil) {
        let console = ConsoleClient(cli: cli), inboxClient = InboxClient(cli: cli)
        client = console
        inbox = InboxModel(client: inboxClient, now: now)
        overview = OverviewModel(console: console, inbox: inboxClient)
        runs = RunsModel(client: console)
        quota = QuotaPanelModel(client: console)
        chairs = ChairsModel(client: console)
        search = SearchModel(client: console)
        attempt = AttemptModel(client: console)
        self.defaults = defaults
        let saved = defaults?.string(forKey: Self.defaultsKey).flatMap(Destination.init(rawValue:))
        item = SidebarItem(saved.flatMap { $0 == .search ? nil : $0 } ?? .overview)
    }

    // MARK: Navigation

    func select(_ item: SidebarItem) {
        guard let destination = updateSelection(item) else { return }
        Task { await load(destination) }
    }

    /// Applies navigation without deciding when its screen should read. Deep links use this to select a screen,
    /// then await one fresh read before selecting the linked row.
    private func updateSelection(_ item: SidebarItem) -> Destination? {
        self.item = item
        // Request search belongs to the Inbox; elsewhere the field defaults to searching results.
        if item.destination == .inbox { if searchScope != .requests, searchText.isEmpty { searchScope = .requests } }
        else if item.destination != .search, searchScope == .requests { searchScope = .results }
        if case .inbox(let scope) = item { inbox.show(scope) }
        if item.destination != .search { defaults?.set(item.destination.rawValue, forKey: Self.defaultsKey) }
        return stale.contains(item.destination) ? item.destination : nil
    }

    func show(_ destination: Destination) {
        // Returning to the Inbox keeps the filter or project the person chose there.
        if destination == .inbox, self.destination == .inbox { return }
        select(destination == .inbox ? .inbox(inbox.scope) : SidebarItem(destination))
    }

    /// Opens the inspector on an attempt.
    func showAttempt(_ id: String) {
        inspectorShown = true
        Task { await attempt.show(id) }
    }

    /// Opens a live request from a menu-bar link against a fresh inbox read.
    func openRequest(_ id: String) async {
        searchText = ""
        stale.insert(.inbox)
        _ = updateSelection(.inbox(.all))
        await load(.inbox)
        if inbox.visibleRequests.contains(where: { $0.id == id }) { inbox.selection = id }
    }

    /// Opens an attempt in the inspector and brings its Runs context forward.
    func openAttempt(_ id: String) async {
        stale.insert(.runs)
        _ = updateSelection(.runs)
        inspectorShown = true
        async let runsRead: Void = load(.runs)
        await attempt.show(id)
        await runsRead
    }

    // MARK: Reading

    /// Reads the screen in front and marks the rest out of date, for ⌘R and for the app coming to the front.
    func refresh() async {
        stale = Set(Destination.allCases)
        await load(destination)
        if inspectorShown { await attempt.reload() }
    }

    var refreshing: Bool {
        switch destination {
        case .overview: overview.reader.loading
        case .inbox: inbox.refreshing
        case .runs: runs.isLoading
        case .quota: quota.reader.loading
        case .chairs: chairs.reader.loading
        case .search: search.reader.loading
        }
    }

    func load(_ destination: Destination) async {
        stale.remove(destination)
        switch destination {
        case .overview: await overview.refresh()
        case .inbox: await inbox.refresh()
        case .runs: await runs.refresh()
        case .quota: await quota.refresh()
        case .chairs: await chairs.refresh()
        case .search: await search.rerun()
        }
    }

    /// The change watcher's answer. A scope that moved, or is unknown (nil), marks the screens it feeds out of date,
    /// and the screen in front reads again at once.
    func apply(_ changes: ChangedScopes) {
        func moved(_ value: Bool?) -> Bool { value != false }
        var hit = Set<Destination>()
        if moved(changes.inbox) { hit.formUnion([.inbox, .overview]) }
        if moved(changes.runs) || moved(changes.board) { hit.formUnion([.runs, .overview, .search]) }
        if moved(changes.chair) { hit.insert(.chairs) }
        if moved(changes.quota) { hit.insert(.quota) }
        if moved(changes.configuration) { hit.formUnion([.quota, .chairs]) }
        stale.formUnion(hit)
        if stale.contains(destination) { Task { await load(destination) } }
        if inspectorShown, moved(changes.runs) || moved(changes.board) { Task { await attempt.reload() } }
    }

    // MARK: Search

    /// Return in the search field. Request search is live already; prompt and result search read the CLI.
    func submitSearch() {
        guard let part = searchScope.part else {
            if destination != .inbox { select(.inbox(inbox.scope)) }
            return
        }
        guard !searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        select(.search)
        Task { await search.run(searchText, part: part) }
    }

    func beginSearch() { searching = true }

    /// Request search narrows the Inbox as it is typed; the other scopes leave it alone.
    private func syncInboxSearch() {
        inbox.searchText = searchScope == .requests ? searchText : ""
    }
}

extension ConsoleModel {
    /// The title and state for an attempt, from its board row or else its overview row.
    func heading(_ id: String) -> (title: String, state: String, outcome: String?)? {
        if let lane = runs.board.value?.lanes.first(where: { $0.id == id }) {
            return (lane.title, lane.metadata.attempt.state.state, lane.metadata.attempt.state.outcome)
        }
        return overview.reader.value?.overview.lanes.first { $0.id == id }.map { ($0.title, $0.state, $0.outcome) }
    }
}
