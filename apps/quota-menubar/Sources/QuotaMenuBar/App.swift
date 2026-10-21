import SwiftUI

@main
struct QuotaMenuBarApp: App {
    @State private var model = QuotaModel()
    init() { NSApplication.shared.setActivationPolicy(.accessory) }   // no Dock icon or app menu

    var body: some Scene {
        MenuBarExtra { QuotaPanel(model: model) } label: { MenuBarLabel(model: model) }
            .menuBarExtraStyle(.window)
    }
}

/// A template gauge symbol chosen by level, plus monospaced digits.
struct MenuBarLabel: View {
    let model: QuotaModel

    var body: some View {
        let report = model.report, used = report?.headline(now: .now)
        let levels = ["0", "33", "50", "67", "100"], index = [20, 45, 60, 100].firstIndex { (used ?? 0) < Double($0) } ?? 4
        HStack(spacing: 4) {
            Image(systemName: model.error != nil ? "exclamationmark.triangle" : "gauge.with.dots.needle.\(levels[index])percent")
            Text(report.map { $0.enabled ? used.map(Fmt.percent) ?? "–" : "Off" } ?? "–").monospacedDigit()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(["Provider quota", used.map { "highest use \(Int($0.rounded(.down))) percent" },
                             report.flatMap { $0.exhausted.isEmpty ? nil : "\($0.exhausted.count) exhausted" },
                             report.flatMap { $0.unavailable.isEmpty ? nil : "\($0.unavailable.count) unavailable" },
                             model.error.map { _ in "last refresh failed" }].compactMap { $0 }.joined(separator: ", "))
    }
}

struct QuotaPanel: View {
    let model: QuotaModel

    var body: some View {
        TimelineView(.everyMinute) { context in   // keeps relative times current while the panel is open
            VStack(alignment: .leading, spacing: 12) {
                header(now: context.date)
                if let error = model.error {
                    Label(model.report == nil ? error : "\(error) Showing the last report.", systemImage: "exclamationmark.triangle.fill")
                        .symbolRenderingMode(.multicolor).font(.caption).textSelection(.enabled)
                        .padding(8).frame(maxWidth: .infinity, alignment: .leading)
                        .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                }
                content(now: context.date)
                Divider()
                HStack {
                    Text(model.report?.host ?? "").foregroundStyle(.tertiary)
                    Spacer()
                    Button("Quit Quota") { NSApplication.shared.terminate(nil) }.keyboardShortcut("q").buttonStyle(.borderless)
                }
                .font(.callout)
            }
            .padding(14)
            .frame(width: 300)
        }
        .onAppear { model.refreshIfStale() }
    }

    private func header(now: Date) -> some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Provider Quota").font(.headline)
                Text(model.updatedAt.map { "Updated \(Fmt.ago($0, now: now))" } ?? (model.error == nil ? "Loading…" : "Not updated")).note()
            }
            Spacer()
            Button(action: model.refresh) {
                if model.isRefreshing { ProgressView().controlSize(.mini) } else { Image(systemName: "arrow.clockwise") }
            }
            .buttonStyle(.borderless).keyboardShortcut("r").disabled(model.isRefreshing)
            .help("Refresh (⌘R)").accessibilityLabel(model.isRefreshing ? "Refreshing" : "Refresh")
        }
    }

    @ViewBuilder private func content(now: Date) -> some View {
        if let report = model.report {
            if !report.enabled {
                Text("Quota fetching is turned off in Provenant's quota settings.").note()
            } else if report.accounts.isEmpty {
                Text("No quota accounts are configured.").note()
            }
            ForEach(report.enabled ? report.byProvider : [], id: \.provider) { group in
                let single = group.accounts.count == 1
                // An error every account shares (say, missing credentials) is shown once under the heading.
                let errors = Set(group.accounts.map { $0.hasReading ? nil : $0.error ?? "Not reported" })
                let shared = errors.count == 1 && !single ? errors.first! : nil
                VStack(alignment: .leading, spacing: 6) {
                    HStack(alignment: .firstTextBaseline) {   // one account: its metadata shares the heading line
                        Text(Fmt.human(group.provider)).font(.headline)
                        Spacer()
                        if single { Text(group.accounts[0].meta(named: false, now: now)).note() }
                    }
                    if let shared { Label(shared, systemImage: "exclamationmark.circle").note() }
                    ForEach(group.accounts) { AccountView(account: $0, named: !single, showError: shared == nil, now: now) }
                }
            }
        } else if model.error == nil {
            ProgressView().controlSize(.small).frame(maxWidth: .infinity)
        }
    }
}

struct AccountView: View {
    let account: QuotaReport.Account, named: Bool, showError: Bool, now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if named { Text(account.meta(named: true, now: now)).note() }
            if account.status == "stale" { Label("The latest fetch failed; this reading is older.", systemImage: "clock").note() }
            if !account.hasReading && showError { Label(account.error ?? "Not reported", systemImage: "exclamationmark.circle").note() }
            ForEach(account.windows) { WindowRow(window: $0, now: now) }
            if let missing = account.unreported, !missing.isEmpty {
                Text("\(missing.map(Fmt.human).joined(separator: ", ")) not reported").note()
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(Fmt.human(account.provider)) \(account.account)")
    }
}

struct WindowRow: View {
    let window: QuotaReport.Window, now: Date

    var body: some View {
        let used = window.usedPercent, expired = window.resetAt.map { $0 <= now } ?? false
        let state = expired ? "unknown" : window.pace.state
        let runsOut = state == "deficit" ? Fmt.until(window.pace.runsOutAt, now: now) : nil
        let left = expired ? "Reset passed; awaiting a new reading" : runsOut.map { "Runs out in \($0)" } ?? Fmt.paceLabel(state)
        let right = expired ? nil : Fmt.until(window.resetAt, now: now).map { "Resets in \($0)" }
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline) {
                Text(Fmt.windowTitle(window))
                Spacer()
                Text(used.map(Fmt.percent) ?? "–").monospacedDigit().fontWeight(.medium)
                    .foregroundStyle(["exhausted", "deficit"].contains(state) ? tint(state) : .primary)
            }
            Capsule().fill(.quaternary).frame(height: 5).overlay(alignment: .leading) {
                GeometryReader { Capsule().fill(tint(state)).frame(width: $0.size.width * min(max(used ?? 0, 0), 100) / 100) }
            }
            HStack { Text(left); Spacer(); Text(right ?? "").monospacedDigit() }.note()
        }
        .help(window.resetAt.map { "Resets \($0.formatted(.dateTime.weekday().day().month().hour().minute()))" } ?? "Reset time not reported")
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Fmt.windowTitle(window))
        .accessibilityValue([used.map { "\(Int($0.rounded(.down))) percent used" }, left, right].compactMap { $0 }.joined(separator: ", "))
    }

    /// Colour only where it carries meaning; system colours adapt to light and dark.
    private func tint(_ state: String) -> Color {
        switch state {
        case "exhausted": return .red
        case "deficit": return .orange
        case "on_pace", "reserve", "early", "not_started": return .green
        default: return .secondary
        }
    }
}

extension QuotaReport.Account {
    /// Account name (when a provider has several), plan, inactivity, and the reading's age once over 30 minutes.
    func meta(named: Bool, now: Date) -> String {
        let observed = observedAt.flatMap { now.timeIntervalSince($0) > 1800 ? "Observed \(Fmt.ago($0, now: now))" : nil }
        return [named ? account : nil, plan.map(Fmt.human), active == false ? "Inactive" : nil, observed]
            .compactMap { $0 }.joined(separator: " · ")
    }
}

extension View {
    func note() -> some View { font(.caption).foregroundStyle(.secondary) }
}
