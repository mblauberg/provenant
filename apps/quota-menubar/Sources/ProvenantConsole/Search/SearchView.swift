import ProvenantDesign
import ProvenantKit
import SwiftUI

/// Matches of `console search`: the first match in each prompt or result.
struct SearchView: View {
    @Bindable var console: ConsoleModel
    private var model: SearchModel { console.search }

    var body: some View {
        if let results = model.reader.value {
            List(selection: Binding<String?>(get: { nil }, set: { id in
                if let id, let match = results.matches.first(where: { $0.id == id }) { console.showAttempt(match.laneId.attemptId) }
            })) {
                if results.truncated { Notice("Only the first matches are shown.", tone: .neutral) }
                ForEach(results.matches) { match in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(match.snippet).lineLimit(3)
                        Text("\(match.part.capitalized) of attempt \(match.laneId.attemptId.prefix(8))")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    .tag(match.id)
                    .accessibilityElement(children: .combine)
                }
            }
            .listStyle(.inset)
            .overlay {
                if results.matches.isEmpty {
                    EmptyPanel("No Matches", symbol: "magnifyingglass", message: "No \(results.part) contains “\(results.query)”.")
                }
            }
        } else if let error = model.reader.error {
            ErrorPanel("Search Failed", message: error) { Task { await model.rerun() } }
        } else if model.reader.loading {
            LoadingPanel("Searching…")
        } else {
            EmptyPanel("Search Prompts and Results", symbol: "magnifyingglass",
                       message: "Type in the search field, choose Prompts or Results, and press Return.")
        }
    }
}
