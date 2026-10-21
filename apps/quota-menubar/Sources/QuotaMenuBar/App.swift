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
        let level = ["0", "33", "50", "67", "100"][[20, 45, 60, 100].firstIndex { (used ?? 0) < Double($0) } ?? 4]
        let exhausted = report?.exhausted(now: .now).count ?? 0, unavailable = report?.unavailable.count ?? 0
        HStack(spacing: 4) {
            Image(systemName: model.error != nil ? "exclamationmark.triangle" : "gauge.with.dots.needle.\(level)percent")
            Text(report.map { $0.enabled ? used.map(Fmt.percent) ?? "–" : "Off" } ?? "–").monospacedDigit()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(["Provider quota", used.map { "highest use \(Int($0.rounded(.down))) percent" },
                             exhausted > 0 ? "\(exhausted) exhausted" : nil, unavailable > 0 ? "\(unavailable) unavailable" : nil,
                             model.error.map { _ in "last refresh failed" }].compactMap { $0 }.joined(separator: ", "))
    }
}

/// The panel: a summary gauge, one menu-style row per account, and menu-style footer items.
struct QuotaPanel: View {
    let model: QuotaModel
    @State var expanded: Set<String> = []

    var body: some View {
        TimelineView(.everyMinute) { context in   // keeps relative times current while the panel is open
            let now = context.date, report = model.report
            let accounts = report.map { $0.enabled ? $0.sorted(now: now) : [] } ?? []
            VStack(alignment: .leading, spacing: 8) {
                Summary(report: report, error: model.error, now: now).padding([.horizontal, .top], 8)
                if let error = model.error, report != nil {
                    Label("\(error) Showing the last report.", systemImage: "exclamationmark.triangle.fill")
                        .symbolRenderingMode(.multicolor).font(.subheadline).foregroundStyle(.secondary)
                        .textSelection(.enabled).padding(.horizontal, 8)
                }
                if !accounts.isEmpty {
                    Separator()
                    ScrollView { rows(accounts, now: now) }   // a tall report scrolls instead of overrunning the screen
                        .frame(maxHeight: 560).fixedSize(horizontal: false, vertical: true).scrollBounceBehavior(.basedOnSize)
                }
                Separator()
                VStack(alignment: .leading, spacing: 0) {
                    Text(model.updatedAt.map { "Updated \(Fmt.ago($0, now: now))" } ?? "Not updated yet")
                        .font(.subheadline).foregroundStyle(.secondary).padding(.horizontal, 8).padding(.bottom, 4)
                    MenuRow(shortcut: "r", action: model.refresh) { Text(model.isRefreshing ? "Refreshing…" : "Refresh") }
                        .disabled(model.isRefreshing)
                    MenuRow(shortcut: "q", action: { NSApplication.shared.terminate(nil) }) { Text("Quit Quota") }
                }
            }
            .padding(6)
            .frame(width: 300)
        }
        .onAppear { model.refreshIfStale() }
    }

    private func rows(_ accounts: [QuotaReport.Account], now: Date) -> some View {
        let sections = [("Available", 0), ("Unavailable", 1), ("Exhausted", 2)]
            .map { title, rank in (title, accounts.filter { $0.rank(now) == rank }) }.filter { !$0.1.isEmpty }
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(sections, id: \.0) { title, members in
                // A quiet menu section header names the state, so dimming is never the only cue.
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                    .padding(.horizontal, 8).padding(.top, title == sections[0].0 ? 0 : 8).padding(.bottom, 2)
                ForEach(members) { account in
                    AccountRow(account: account, now: now, expanded: Binding(
                        get: { expanded.contains(account.id) },
                        set: { if $0 { expanded.insert(account.id) } else { expanded.remove(account.id) } }))
                }
            }
        }
    }
}

struct Summary: View {
    let report: QuotaReport?, error: String?, now: Date

    var body: some View {
        let top = report?.top(now: now), used = report?.headline(now: now)
        let firstBack = report?.exhausted(now: now).compactMap { $0.binding(now: now)?.resetAt }.min()
        let (title, detail): (String, String) =
            report == nil ? (error == nil ? ("Loading…", "") : ("Quota unavailable", error!))
            : !report!.enabled ? ("Quota is off", "Turned off in Provenant's quota settings.")
            : report!.accounts.isEmpty ? ("No accounts", "No quota accounts are configured.")
            : top.map { ($0.account.title, Fmt.status($0.window, now: now)) }
            ?? (used == nil ? ("No current readings", "") : ("All accounts exhausted", "First back in \(Fmt.until(firstBack, now: now) ?? "–")"))
        HStack(spacing: 12) {
            Ring(value: used, state: top?.window.pace.state ?? (used == nil ? "unknown" : "exhausted"), size: 40)
                .overlay { Text(used.map(Fmt.percent) ?? "–").font(.system(size: 11, weight: .semibold)).monospacedDigit() }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                if !detail.isEmpty { Text(detail).font(.subheadline).foregroundStyle(.secondary).lineLimit(3).textSelection(.enabled) }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel([used.map { "Highest use \(Int($0.rounded(.down))) percent" }, title, detail].compactMap { $0 }.joined(separator: ", "))
    }
}

/// One account as a menu item; clicking it discloses every window.
struct AccountRow: View {
    let account: QuotaReport.Account, now: Date
    @Binding var expanded: Bool

    var body: some View {
        let window = account.binding(now: now), usable = account.rank(now) == 0
        let aged = account.status == "stale" || account.observedAt.map { now.timeIntervalSince($0) > 1800 } ?? false
        let detail = account.hasReading ? window.map { Fmt.status($0, now: now) } ?? "No current reading" : account.error ?? "Not reported"
        VStack(alignment: .leading, spacing: 0) {
            MenuRow(action: { withAnimation(.snappy(duration: 0.2)) { expanded.toggle() } }) {
                HStack(spacing: 8) {
                    Ring(value: window?.usedPercent, state: window?.pace.state ?? "unknown", size: 22)
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 4) {
                            Text(account.title)
                            if aged {
                                Image(systemName: "clock").font(.caption).foregroundStyle(.secondary)
                                    .help(account.observedAt.map { "Observed \(Fmt.ago($0, now: now))" } ?? "Reading is stale")
                            }
                        }
                        Text(detail).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    if usable, let used = window?.usedPercent { Text(Fmt.percent(used)).monospacedDigit().foregroundStyle(.secondary) }
                }
                .opacity(usable ? 1 : 0.55)   // exhausted and unreadable accounts recede
            }
            .accessibilityLabel([account.title, window?.usedPercent.map { "\(Int($0.rounded(.down))) percent used" }, detail]
                .compactMap { $0 }.joined(separator: ", "))
            .accessibilityHint(expanded ? "Hides the windows" : "Shows every window")
            if expanded { WindowList(account: account, now: now).padding(.leading, 38).padding([.trailing, .bottom], 8) }
        }
    }
}

struct WindowList: View {
    let account: QuotaReport.Account, now: Date

    var body: some View {
        let missing = (account.unreported ?? []) + account.windows.filter { $0.usedPercent == nil }.map(\.name)
        let meta = [account.plan.map(Fmt.human), account.observedAt.map { "observed \(Fmt.ago($0, now: now))" },
                    account.status == "stale" ? "latest fetch failed" : nil].compactMap { $0 }
        VStack(alignment: .leading, spacing: 4) {
            ForEach(account.windows.filter { $0.usedPercent != nil }) { window in
                HStack(spacing: 6) {
                    let expired = window.resetAt.map { $0 <= now } ?? false
                    Ring(value: window.usedPercent, state: expired ? "unknown" : window.pace.state, size: 10)
                    Text(Fmt.status(window, now: now)).lineLimit(1)
                    Spacer(minLength: 4)
                    Text(Fmt.percent(window.usedPercent ?? 0)).monospacedDigit()
                }
                .help(window.resetAt.map { "Resets \($0.formatted(.dateTime.weekday().day().month().hour().minute()))" } ?? "Reset time not reported")
                .accessibilityElement(children: .combine)
            }
            if !missing.isEmpty {   // a missing window never means 0%
                Text("\(missing.count) more not reported").help(missing.map(Fmt.human).joined(separator: ", "))
            }
            if !meta.isEmpty { Text(meta.joined(separator: " · ")).foregroundStyle(.tertiary) }
        }
        .font(.subheadline).foregroundStyle(.secondary)
    }
}

/// A capacity ring. Colour is a small tint that carries pace meaning; system colours adapt to light and dark.
struct Ring: View {
    let value: Double?, state: String, size: CGFloat

    var body: some View {
        let width = max(size / 8, 2)
        ZStack {
            Circle().inset(by: width / 2).stroke(.quaternary, lineWidth: width)
            Circle().inset(by: width / 2).trim(from: 0, to: min(max((value ?? 0) / 100, 0), 1))
                .stroke(tint, style: StrokeStyle(lineWidth: width, lineCap: .round)).rotationEffect(.degrees(-90))
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    private var tint: Color {
        ["exhausted": .red, "deficit": .orange, "on_pace": .green, "reserve": .green, "early": .green, "not_started": .green][state]
            ?? .secondary
    }
}

/// A menu item: full-width target, the rounded hover highlight macOS menus use, and its shortcut shown trailing.
struct MenuRow<Label: View>: View {
    var shortcut: KeyEquivalent? = nil
    let action: () -> Void
    @ViewBuilder let label: Label
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack {
                label
                Spacer(minLength: 0)
                if let shortcut { Text("⌘\(String(shortcut.character).uppercased())").foregroundStyle(.secondary) }
            }
            .padding(.horizontal, 8).padding(.vertical, 4).contentShape(Rectangle())
            .background(.quaternary.opacity(hovering ? 1 : 0), in: RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .keyboardShortcut(shortcut.map { KeyboardShortcut($0) })
        .onHover { hovering = $0 }
    }
}

/// An inset menu separator.
struct Separator: View {
    var body: some View { Divider().padding(.horizontal, 8) }
}
