import ProvenantDesign
import ProvenantKit
import SwiftUI

struct ChairsView: View {
    let model: ChairsModel
    let now: Date

    var body: some View {
        if let snapshot = model.reader.value {
            List {
                if let error = model.reader.error { Notice("Refresh failed. \(error)") }
                Section("Chair sessions") {
                    if snapshot.sessions.isEmpty { Text("No chair sessions are registered.").foregroundStyle(.secondary) }
                    ForEach(snapshot.sessions) { SessionRow(session: $0, project: snapshot.projectName($0.record.projectId), now: now) }
                    if snapshot.truncated { Text("More sessions exist than are shown.").font(.caption).foregroundStyle(.secondary) }
                }
                Section("Projects") {
                    if snapshot.projects.isEmpty { Text("No projects are registered.").foregroundStyle(.secondary) }
                    ForEach(snapshot.projects) { project in
                        VStack(alignment: .leading, spacing: 2) {
                            Label(project.label ?? "Unnamed project", systemImage: "folder")
                            if let path = project.commonDir?.path {
                                Text(path).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                            }
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
            .listStyle(.inset)
        } else if let error = model.reader.error {
            ErrorPanel("Can’t Read Chairs", message: error) { Task { await model.refresh() } }
        } else {
            LoadingPanel("Reading chairs and projects…")
        }
    }
}

private struct SessionRow: View {
    let session: ChairSession
    let project: String?
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Label(summaryLine, systemImage: "person.crop.circle")
            Text(detailLine).font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var summaryLine: String {
        session.summary?.now ?? (session.summaryStatus == "available" ? "Session \(session.id.prefix(8))" : "No summary yet")
    }

    private var detailLine: String {
        [project, "Session \(session.id.prefix(8))", session.summary?.next.map { "Next: \($0)" },
         session.summary?.risk.flatMap { $0 == "none" ? nil : "Risk: \($0)" },
         session.record.lastSavedAt.map { "saved \(Stamp.relative($0, now: now))" }]
            .compactMap { $0 }.joined(separator: " · ")
    }
}
