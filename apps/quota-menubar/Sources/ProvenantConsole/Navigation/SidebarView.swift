import ProvenantKit
import SwiftUI

/// Two levels: the destinations, and under Inbox its filters and projects.
struct SidebarView: View {
    @Bindable var console: ConsoleModel

    var body: some View {
        List(selection: Binding<SidebarItem?>(get: { console.item }, set: { if let item = $0 { console.select(item) } })) {
            Label("Overview", systemImage: Destination.overview.symbol)
                .tag(SidebarItem.overview)
                .badge(console.overview.reader.value?.needsYou.count ?? 0)
                .accessibilityLabel(overviewLabel)
            DisclosureGroup(isExpanded: $console.inboxExpanded) {
                InboxSidebarRows(model: console.inbox)
            } label: {
                Label("Inbox", systemImage: Destination.inbox.symbol)
                    .badge(console.inbox.count(for: .all))
                    .tag(SidebarItem.inbox(.all))
                    .accessibilityLabel("Inbox, \(console.inbox.count(for: .all)) open")
            }
            Label("Runs", systemImage: Destination.runs.symbol).tag(SidebarItem.runs)
            Label("Quota", systemImage: Destination.quota.symbol).tag(SidebarItem.quota)
            Label("Chairs & Projects", systemImage: Destination.chairs.symbol).tag(SidebarItem.chairs)
            if !console.search.query.isEmpty {
                Label("Search Results", systemImage: Destination.search.symbol).tag(SidebarItem.search)
            }
        }
        .listStyle(.sidebar)
        .accessibilityLabel("Sections")
    }

    private var overviewLabel: String {
        let count = console.overview.reader.value?.needsYou.count ?? 0
        return count == 0 ? "Overview" : "Overview, \(count) need you"
    }
}
