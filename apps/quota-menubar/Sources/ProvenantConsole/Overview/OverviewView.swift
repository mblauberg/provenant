import ProvenantDesign
import ProvenantKit
import SwiftUI

struct OverviewView: View {
    @Bindable var console: ConsoleModel
    private var reader: Reader<OverviewSnapshot> { console.overview.reader }

    var body: some View {
        Group {
            if let snapshot = reader.value {
                content(snapshot)
            } else if let error = reader.error {
                ErrorPanel("Can’t Read the Overview", message: error) { Task { await console.load(.overview) } }
            } else {
                LoadingPanel("Reading the overview…")
            }
        }
    }

    private func content(_ snapshot: OverviewSnapshot) -> some View {
        let counts = snapshot.overview.counts
        return VStack(spacing: 0) {
            HStack(alignment: .top, spacing: Space.large) {
                Metric(Self.count(counts.needsYou ?? UInt64(snapshot.needsYou.count)), "Needs you", symbol: "tray.full",
                       tone: snapshot.needsYou.isEmpty ? .neutral : .caution)
                Metric(Self.count(counts.running), "Running", symbol: "bolt.horizontal", tone: .active)
                Metric(Self.count(counts.attention), "Needs attention", symbol: "exclamationmark.triangle",
                       tone: (counts.attention ?? 0) > 0 ? .caution : .neutral)
            }
            .padding(Space.large)
            ForEach(snapshot.warnings, id: \.self) { Notice($0, tone: .neutral) }
            if let error = reader.error { Notice("Refresh failed. \(error)") }
            list(snapshot)
        }
    }

    private func list(_ snapshot: OverviewSnapshot) -> some View {
        List {
            Section("Needs you") {
                if snapshot.needsYou.isEmpty {
                    Text("Nothing is waiting on you.").foregroundStyle(.secondary)
                }
                ForEach(snapshot.needsYou) { row in
                    Button { open(row) } label: { NeedsYouLine(row: row) }
                        .buttonStyle(.plain)
                }
            }
            Section("Lanes in progress") {
                if snapshot.overview.lanes.isEmpty {
                    Text(snapshot.overview.coverage.runs == "complete" ? "No lanes are running." : "Run state is unavailable here.")
                        .foregroundStyle(.secondary)
                }
                ForEach(snapshot.overview.lanes) { lane in
                    Button { console.showAttempt(lane.id) } label: { LaneRow(lane) }
                        .buttonStyle(.plain)
                }
            }
        }
        .listStyle(.inset)
    }

    private func open(_ row: NeedsYouRow) {
        console.inbox.show(.all)
        console.inbox.selection = row.id
        console.show(.inbox)
    }

    /// A count, or a dash when the CLI could not count it.
    static func count(_ value: UInt64?) -> String { value.map(String.init) ?? "–" }
}

private struct NeedsYouLine: View {
    let row: NeedsYouRow

    var body: some View {
        HStack(spacing: Space.small) {
            Image(systemName: row.blocking ? "exclamationmark.octagon.fill" : "questionmark.bubble")
                .foregroundStyle(row.blocking ? Color.orange : Color.secondary)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title)
                Text([row.kind?.title, row.blocking ? "Blocking" : nil, row.project].compactMap { $0 }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary).accessibilityHidden(true)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens the request in the Inbox")
    }
}
