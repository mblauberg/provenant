import ProvenantDesign
import ProvenantKit
import SwiftUI

/// Attempt detail in the window's inspector: status, routes, timings, usage, artifacts, linked requests, and the
/// text of the result or prompt.
struct AttemptInspector: View {
    @Bindable var console: ConsoleModel
    private var model: AttemptModel { console.attempt }

    var body: some View {
        Group {
            if model.attemptID == nil {
                EmptyPanel("No Attempt Selected", symbol: "sidebar.trailing",
                           message: "Choose a run, a lane or a search result to see its detail.")
            } else if let detail = model.detail.value {
                if detail.isFound { found(detail) } else { unavailable(detail) }
            } else if let error = model.detail.error {
                ErrorPanel("Can’t Read the Attempt", message: error) { Task { await model.reload() } }
            } else {
                LoadingPanel("Reading the attempt…")
            }
        }
    }

    private func unavailable(_ detail: AttemptDetail) -> some View {
        EmptyPanel(detail.status == "missing" ? "Attempt Not Found" : "Attempt Unavailable", symbol: "questionmark.folder",
                   message: detail.reason ?? "This host holds no record of attempt \(detail.laneId.attemptId.prefix(8)).")
    }

    private func found(_ detail: AttemptDetail) -> some View {
        let now = console.inbox.displayNow
        let lane = console.lane(detail.laneId.attemptId)
        return ScrollView {
            VStack(alignment: .leading, spacing: Space.large) {
                VStack(alignment: .leading, spacing: Space.tight) {
                    Text(lane?.title ?? "Attempt \(detail.laneId.attemptId.prefix(8))")
                        .font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader).textSelection(.enabled)
                    if let state = lane?.metadata.attempt.state {
                        StatusLabel(.attempt(state: state.state, outcome: state.outcome))
                    }
                }
                routes(detail)
                timings(detail.timings, now: now)
                usage(detail)
                artifacts(detail.artifacts ?? [])
                linked(detail.linkedInboxRequests ?? [])
                outputSection
                trace(detail)
            }
            .padding(Space.medium)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder private func routes(_ detail: AttemptDetail) -> some View {
        let rows: [(String, Route?)] = [("Requested", detail.requestedRoute), ("Resolved", detail.resolvedRoute),
                                        ("Applied", detail.appliedRoute), ("Observed", detail.observedRoute)]
        let known = rows.filter { !($0.1?.summary.isEmpty ?? true) }
        if !known.isEmpty {
            Block("Route") {
                ForEach(known, id: \.0) { Field($0.0, $0.1?.summary ?? "") }
            }
        }
    }

    @ViewBuilder private func timings(_ timings: Timings?, now: Date) -> some View {
        if let timings {
            let rows: [(String, String?)] = [("Queued", timings.queuedAt), ("Started", timings.startedAt), ("Finished", timings.finishedAt)]
            Block("Timing") {
                ForEach(rows.filter { $0.1 != nil }, id: \.0) { name, stamp in
                    Field(name, Stamp.relative(stamp, now: now)).help(Stamp.absolute(stamp))
                }
                if let ms = timings.providerMs { Field("Provider time", Stamp.duration(milliseconds: ms)) }
            }
        }
    }

    @ViewBuilder private func usage(_ detail: AttemptDetail) -> some View {
        let counts = detail.usage?.counts ?? []
        Block("Usage") {
            if counts.isEmpty { Text("No usage was reported.").foregroundStyle(.secondary) }
            ForEach(counts, id: \.label) { Field($0.label, $0.value.formatted()) }
            if let missing = detail.usageCoverage?.missing, !missing.isEmpty, !counts.isEmpty {
                Text("Not reported: \(missing.map { $0.replacingOccurrences(of: "_", with: " ") }.joined(separator: ", "))")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private func artifacts(_ artifacts: [Artifact]) -> some View {
        if !artifacts.isEmpty {
            Block("Artifacts") {
                ForEach(artifacts) { artifact in
                    HStack {
                        Label(artifact.name, systemImage: artifact.present ? "doc.text" : "doc.badge.ellipsis")
                        Spacer(minLength: Space.small)
                        Text(artifact.bytes.map { Int64($0).formatted(.byteCount(style: .file)) } ?? "–")
                            .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(artifact.name), \(artifact.present ? "present" : "not present")")
                }
            }
        }
    }

    @ViewBuilder private func linked(_ requests: [LinkedRequest]) -> some View {
        if !requests.isEmpty {
            Block("Linked requests") {
                ForEach(requests) { request in
                    Button {
                        console.inbox.show(.all)
                        console.inbox.selection = request.id
                        console.show(.inbox)
                    } label: {
                        Label(request.title ?? request.id, systemImage: "tray")
                    }
                    .buttonStyle(.link)
                }
            }
        }
    }

    private var outputSection: some View {
        Block("Text") {
            Picker("Part", selection: Binding(get: { model.part }, set: { part in Task { await model.setPart(part) } })) {
                ForEach(AttemptModel.Part.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .accessibilityLabel("Show the result or the prompt")
            if let text = model.output.value?.text, !text.isEmpty {
                Text(text).font(.callout.monospaced()).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if model.output.loading {
                ProgressView().controlSize(.small)
            } else {
                Text(model.output.error ?? "No \(model.part.rawValue) text is recorded yet.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private func trace(_ detail: AttemptDetail) -> some View {
        if let trace = detail.trace {
            Block("Trace") {
                ForEach([("Run", trace.run), ("Attempt", trace.attempt), ("Parent", trace.parent), ("Operation", trace.operation)], id: \.0) { name, id in
                    if let id { Field(name, id) }
                }
            }
        }
    }
}
