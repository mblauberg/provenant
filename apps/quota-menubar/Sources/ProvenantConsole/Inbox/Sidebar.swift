import ProvenantKit
import SwiftUI

/// The Inbox's second sidebar level: smart filters, then the projects that have open requests.
struct InboxSidebarRows: View {
    let model: InboxModel

    var body: some View {
        ForEach([InboxFilter.blocking, .decisions, .questions, .assumptions]) { filter in
            row(filter)
        }
        ForEach(model.projectGroups) { project in
            Label(project.name, systemImage: "folder")
                .badge(project.count)
                .tag(SidebarItem.inbox(.project(project.id)))
                .accessibilityLabel("\(project.name), \(project.count) \(project.count == 1 ? "open request" : "open requests")")
        }
    }

    private func row(_ filter: InboxFilter) -> some View {
        let count = model.count(for: filter)
        return Label(filter.title, systemImage: filter.symbol)
            .badge(count)
            .tag(SidebarItem.inbox(.filter(filter)))
            .accessibilityLabel("\(filter.title), \(count) \(count == 1 ? "open request" : "open requests")")
    }
}
