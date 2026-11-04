import Charts
import ProvenantDesign
import ProvenantKit
import SwiftUI

struct QuotaView: View {
    let model: QuotaPanelModel

    var body: some View {
        if let snapshot = model.reader.value {
            List {
                if let error = model.reader.error { Notice("Refresh failed. \(error)") }
                if !snapshot.historyAvailable { Notice("Usage history could not be read, so trends are hidden.", tone: .neutral) }
                ForEach(snapshot.rows) { row in
                    QuotaRowView(row: row, now: snapshot.generatedAt)
                        .padding(.vertical, Space.tight)
                }
            }
            .listStyle(.inset)
            .overlay {
                if snapshot.rows.isEmpty {
                    EmptyPanel("No Accounts", symbol: "gauge.with.dots.needle.0percent",
                               message: "Register an account with `provenant accounts register --plan` and it appears here.")
                }
            }
        } else if let error = model.reader.error {
            ErrorPanel("Can’t Read Quota", message: error) { Task { await model.refresh() } }
        } else {
            LoadingPanel("Reading quota…")
        }
    }
}

private struct QuotaRowView: View {
    let row: QuotaRow
    let now: Date

    private var tone: Tone {
        guard let binding = row.binding else { return .neutral }
        if row.status == "exhausted" || binding.percent >= 100 { return .bad }
        return binding.percent >= 90 ? .caution : .good
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.small) {
            HStack(alignment: .firstTextBaseline) {
                Text(row.title).font(.headline)
                Text(row.provider).font(.caption).foregroundStyle(.secondary)
                Spacer(minLength: Space.small)
                if let binding = row.binding {
                    Text(binding.value)
                        .accessibilityLabel("\(binding.lowerBound ? "at least " : "")\(binding.displayPercent) percent used").font(.title3.weight(.semibold)).monospacedDigit()
                        .foregroundStyle(tone == .good ? Color.primary : tone.color)
                } else {
                    Text(row.status.replacingOccurrences(of: "_", with: " ").capitalized).foregroundStyle(.secondary)
                }
            }
            ForEach(row.refusals, id: \.id) { refusal in
                let pool = refusal.pool.map { " in pool \($0)" } ?? ""
                Text("Refusal recorded for \(refusal.model)\(pool)\(refusal.resetsAt.map { " until \($0.formatted(date: .abbreviated, time: .shortened))" } ?? "")")
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(row.windows, id: \.pool) { window in
                VStack(alignment: .leading, spacing: 2) {
                    Gauge(value: min(window.percent, 100), in: 0...100) { Text(window.pool) }
                        .gaugeStyle(.accessoryLinearCapacity)
                        .tint(window.percent >= 90 ? Tone.caution.color : Tone.good.color)
                    HStack {
                        Text(window.pool.replacingOccurrences(of: "-", with: " "))
                        Spacer(minLength: Space.small)
                        if let reset = window.resetsAt { Text("resets \(Stamp.relative(reset, now: now))") }
                    }
                    .font(.caption).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("\(window.pool), \(window.lowerBound ? "at least " : "")\(window.displayPercent) percent used\(window.resetsAt.map { ", resets " + Stamp.relative($0, now: now) } ?? "")")
            }
            if let error = row.error { Text(error).font(.caption).foregroundStyle(.secondary) }
            if row.trend.count > 1 { trend }
        }
        .accessibilityElement(children: .contain)
    }

    private var trend: some View {
        Chart(row.trend) { point in
            LineMark(x: .value("Time", point.date), y: .value("Used", point.percent))
                .interpolationMethod(.monotone)
                .foregroundStyle(tone == .good ? Color.accentColor : tone.color)
        }
        .chartYScale(domain: 0...100)
        .chartYAxis { AxisMarks(values: [0, 50, 100]) { AxisGridLine(); AxisValueLabel() } }
        .frame(height: 64)
        .accessibilityLabel("Usage trend, from \(Int(row.trend.first?.percent.rounded() ?? 0)) to \(Int(row.trend.last?.percent.rounded() ?? 0)) percent")
    }
}
