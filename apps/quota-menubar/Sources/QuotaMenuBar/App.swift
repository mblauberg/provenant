import SwiftUI

/// The menu bar app, or `--render <report.json> <out.png>` to draw the panel from a saved report (Render.swift).
@main
enum Main {
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if arguments.first == "--render" { exit(MainActor.assumeIsolated { Render.run(Array(arguments.dropFirst())) }) }
        QuotaMenuBarApp.main()
    }
}

struct QuotaMenuBarApp: App {
    @State private var model = QuotaModel()
    init() { NSApplication.shared.setActivationPolicy(.accessory) }   // no Dock icon or app menu

    var body: some Scene {
        MenuBarExtra { QuotaPanel(model: model) } label: { MenuBarLabel(model: model) }
            .menuBarExtraStyle(.window)
    }
}

/// A template gauge symbol and the highest-use available account; the gauge alone when no account is usable.
/// It reads only the model's settled label, so a refresh that changes nothing the status item shows never redraws it.
struct MenuBarLabel: View {
    let model: QuotaModel

    var body: some View {
        let label = model.label
        HStack(spacing: 4) {
            Image(systemName: label.symbol)
            if let text = label.text { Text(text).monospacedDigit() }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label.spoken)
    }
}

/// What the status item shows. Redrawing a status item costs real CPU, so the model replaces this only when it
/// differs.
struct MenuLabel: Equatable {
    var symbol = "gauge.with.dots.needle.0percent", text: String? = "–", spoken = "Provider quota"

    init() {}

    init(report: QuotaReport?, failed: Bool, now: Date) {
        let featured = report?.featured(now: now), used = featured?.window.usedPercent ?? report?.headline(now: now)
        let level = ["0", "33", "50", "67", "100"][[20, 45, 60, 100].firstIndex { (used ?? 0) < Double($0) } ?? 4]
        symbol = failed ? "exclamationmark.triangle" : "gauge.with.dots.needle.\(level)percent"
        text = report.map { report in
            featured.map { Fmt.percent($0.window.usedPercent ?? 0) }
                ?? (report.iconOnly ? nil : report.headline(now: now).map(Fmt.percent) ?? "–")
        } ?? "–"
        let exhausted = report?.exhausted(now: now).count ?? 0
        let drawing = report?.drawing(now: now).count ?? 0
        let unavailable = report?.accounts.filter { !$0.hasReading && !$0.requiresAuth }.count ?? 0
        spoken = ["Provider quota",
                  featured.map { "\($0.account.title), \($0.window.fullTitle) \(Fmt.percent($0.window.usedPercent ?? 0)) used" },
                  exhausted > 0 ? "\(exhausted) exhausted" : nil,
                  drawing > 0 ? "\(drawing) drawing on credits" : nil, unavailable > 0 ? "\(unavailable) unavailable" : nil,
                  failed ? "last refresh failed" : nil].compactMap { $0 }.joined(separator: ", ")
    }
}

/// The panel: a summary gauge, one menu-style row per account, and menu-style footer items.
struct QuotaPanel: View {
    let model: QuotaModel
    var fixedNow: Date? = nil   // a rendered panel draws one instant
    @State var expanded: Set<String> = []
    /// The visible height of the status item's screen; the account list scrolls inside what is left of it.
    @State var screenHeight: CGFloat = NSScreen.main?.visibleFrame.height ?? 800
    @State private var listHeight: CGFloat = 0
    @State private var chrome: CGFloat = 0   // the panel less its list
    @State private var height: CGFloat = 0   // the whole panel, which the window is fitted to
    @State private var window: NSWindow?   // the menu-bar window, known once it first becomes key
    @State private var open = true   // the panel window outlives a close, so its clock stops when it hides

    var body: some View {
        Group {
            if let fixedNow { content(now: fixedNow) } else if open {
                TimelineView(.everyMinute) { content(now: $0.date) }   // keeps relative times current while the panel is open
            } else { content(now: .now) }
        }
        // Fires on every panel open; .onAppear may fire only once for a menu-bar window.
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { note in
            if let window = note.object as? NSWindow {
                self.window = window
                if let screen = window.screen { screenHeight = screen.visibleFrame.height }
            }
            open = true
            fit()
            model.panelOpened()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didResignKeyNotification)) { _ in open = false }
    }

    private func content(now: Date) -> some View {
        let report = model.report, alert = report?.alertPercent ?? 90
        let accounts = report?.sorted(now: now) ?? []
        let refreshing = model.isRefreshing || report?.isRefreshing(now: now) == true
        let updated = model.updatedAt.map { "Updated \(Fmt.ago($0, now: now))" } ?? "Not updated yet"
        let status = refreshing ? "Refreshing…" : ([updated] + [report?.nextCheck.map { Fmt.next($0, now: now) }].compactMap { $0 })
            .joined(separator: " · ")
        // Short content hugs; a tall report stops a margin short of the screen's edge and scrolls inside.
        let shown = min(listHeight, max(160, screenHeight - 24 - chrome))
        return VStack(alignment: .leading, spacing: 8) {
            Summary(report: report, error: model.error, now: now).padding([.horizontal, .top], 8)
            if let error = model.error, report != nil {
                Label("\(error) Showing the last report.", systemImage: "exclamationmark.triangle.fill")
                    .symbolRenderingMode(.multicolor).font(.subheadline).foregroundStyle(.secondary)
                    .textSelection(.enabled).padding(.horizontal, 8)
            }
            if !accounts.isEmpty {
                Divider().padding(.horizontal, 8)
                ScrollView {
                    rows(accounts, now: now, alert: alert)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { listHeight = $0 }
                }
                .frame(height: shown).scrollBounceBehavior(.basedOnSize)
            }
            Divider().padding(.horizontal, 8)
            VStack(alignment: .leading, spacing: 0) {
                // "Updated just now · next in 4m"; the reason for the next check is on hover.
                Text(status).font(.subheadline).foregroundStyle(.secondary).monospacedDigit()
                    .padding(.horizontal, 8).padding(.bottom, 4)
                    .help(report?.nextCheck.map { Fmt.nextInFull($0, now: now) } ?? "")
                MenuRow(shortcut: "r", action: model.refresh) { Text("Refresh") }
                    .disabled(model.isRefreshing)
                MenuRow(shortcut: "q", action: { NSApplication.shared.terminate(nil) }) { Text("Quit Quota") }
            }
        }
        .padding(6)
        .frame(width: 300)
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { chrome = max(0, $0 - shown); height = $0; fit() }
    }

    /// The menu-bar window grows with the panel but never shrinks, and would centre a collapsed panel in the
    /// expanded height, so the panel sets its height, keeping the top edge under the status item.
    private func fit() {
        guard let window, height > 0, window.frame.height != height else { return }
        var frame = window.frame
        frame.origin.y += frame.height - height
        frame.size.height = height
        window.setFrame(frame, display: true)
    }

    private func rows(_ accounts: [QuotaReport.Account], now: Date, alert: Double) -> some View {
        let sections = [("Available", 0), ("Drawing on credits", 1), ("Unavailable", 2), ("Exhausted", 3), ("Authentication required", 4)]
            .map { title, rank in (title, accounts.filter { $0.rank(now) == rank }) }.filter { !$0.1.isEmpty }
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(sections, id: \.0) { title, members in
                // A quiet menu section header names the state, so dimming is never the only cue.
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                    .padding(.horizontal, 8).padding(.top, title == sections[0].0 ? 0 : 8).padding(.bottom, 2)
                ForEach(members) { account in
                    AccountRow(account: account, now: now, alert: alert, expanded: Binding(
                        get: { expanded.contains(account.id) },
                        set: { if $0 { expanded.insert(account.id) } else { expanded.remove(account.id) } }))
                }
            }
        }
    }
}

/// The headline account: "Claude · Anese" over "Weekly · resets Tue 19 Mar", its use in the ring.
struct Summary: View {
    let report: QuotaReport?, error: String?, now: Date

    var body: some View {
        let alert = report?.alertPercent ?? 90, used = report?.headline(now: now)
        // `none` hides the menu bar figure, but the panel still leads with the most used account.
        let featured = report?.featured(now: now)
            ?? report?.top(now: now).map { ($0.account, $0.account.display(now: now, alert: alert) ?? $0.window) }
        let firstBack = report?.exhausted(now: now).compactMap { $0.back(now: now) }.min()
        let drawing = report?.drawing(now: now) ?? []
        let sources = report?.sources.map { source in
            "\(source.title): \(source.statusTitle)" + (source.error.map { " (\($0))" } ?? "")
        } ?? []
        let coverage = report.map { "\($0.coverage.known) of \($0.coverage.total) quota report rows have current account data." } ?? ""
        let (title, detail): (String, String) = switch (report, featured) {
        case (nil, _): error.map { ("Quota unavailable", $0) } ?? ("Loading…", "")
        case (let report?, _) where report.accounts.isEmpty:
            ("No account snapshots", ([coverage] + sources).joined(separator: "\n"))
        case (_, let featured?): (featured.account.title, [featured.window.fullTitle, Fmt.resets(featured.window.resetAt, now: now)]
                                    .compactMap { $0 }.joined(separator: " · "))
        case _ where !drawing.isEmpty: ("Drawing on credits", drawing.map(\.title).joined(separator: ", "))
        default: used == nil ? ("No current readings", "")
            : ("All accounts exhausted", firstBack.map { "First " + Fmt.back($0, now: now) } ?? "Reset time unknown")
        }
        let estimate = featured.flatMap { Fmt.projection($0.window, now: now) }
        let value = featured?.window.usedPercent ?? used
        HStack(spacing: 12) {
            Ring(value: value, state: featured?.window.pace.state ?? (used == nil ? "unknown" : drawing.isEmpty ? "exhausted" : "credit_only"),
                 size: 40)
                .overlay { Text(value.map(Fmt.percent) ?? "–").font(.system(size: 11, weight: .semibold)).monospacedDigit() }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                if !detail.isEmpty {
                    Text(detail).font(.subheadline).foregroundStyle(.secondary).monospacedDigit().lineLimit(3).textSelection(.enabled)
                }
                if let estimate { Text(estimate).font(.subheadline).foregroundStyle(.tertiary) }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel([title, value.map { "\(Fmt.percent($0)) used" }, detail, estimate].compactMap { $0 }.filter { !$0.isEmpty }
            .joined(separator: ", "))
    }
}

/// One account as a menu item: its rings, its name over one short fact, and its use or reset time.
struct AccountRow: View {
    let account: QuotaReport.Account, now: Date, alert: Double
    @Binding var expanded: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        let usable = account.rank(now) == 0, drawing = account.isDrawing(now: now)
        let aged = !account.requiresAuth && (account.status == .stale || account.observedAt.map { now.timeIntervalSince($0) > 1800 } ?? false)
        let row = Fmt.row(account, now: now, alert: alert)
        let spent = usable ? account.spentPools(now: now).map { Fmt.spent($0, now: now) } : []
        // The binding pool takes the outer ring; its use is the figure on the right.
        let bound = account.binding(now: now)?.pool, rings = account.poolRings(now: now, alert: alert)
        let spoken = [account.title] + row.spoken + spent
            + [aged ? account.status == .stale ? "stale" : account.observedAt.map { "observed \(Fmt.ago($0, now: now))" } : nil].compactMap { $0 }
        VStack(alignment: .leading, spacing: 0) {
            MenuRow(action: { withAnimation(reduceMotion ? nil : .snappy(duration: 0.2)) { expanded.toggle() } }) {
                HStack(spacing: 8) {
                    PoolRings(rings: rings.filter { $0.pool == bound } + rings.filter { $0.pool != bound }, drawing: drawing)
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 4) {
                            Text(account.title).lineLimit(1)
                            if aged {
                                Image(systemName: "clock").font(.caption).foregroundStyle(.secondary)
                                    .help(account.observedAt.map { "Observed \(Fmt.ago($0, now: now))" } ?? "Reading is stale")
                            }
                        }
                        if let fact = Self.fact(row) { fact.font(.subheadline).foregroundStyle(.secondary).monospacedDigit().lineLimit(1) }
                        ForEach(spent, id: \.self) { Text($0).font(.subheadline).foregroundStyle(.secondary).monospacedDigit().lineLimit(1) }
                    }
                    Spacer(minLength: 4)
                    if let trailing = row.trailing {
                        Text(trailing).font(usable ? .body : .subheadline).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                // Exhausted and unavailable accounts recede; less so when the user asks for more contrast.
                .opacity(usable || drawing ? 1 : contrast == .increased ? 0.8 : 0.55)
            }
            .help(account.id)   // fingerprinted accounts can share a title
            .accessibilityLabel(spoken.joined(separator: ", "))
            .accessibilityHint(expanded ? "Hides the windows" : "Shows every window")
            if expanded { WindowList(account: account, now: now).padding(.leading, 38).padding([.trailing, .bottom], 8) }
        }
    }

    /// "Claude models · ↻ Thu 21 Mar": the reset follows a symbol, so the row never repeats "resets".
    static func fact(_ row: Fmt.Row) -> Text? {
        let symbol = Image(systemName: "arrow.counterclockwise")
        switch (row.lead, row.reset) {
        case let (lead?, reset?): return Text("\(lead) · \(symbol) \(reset)")
        case let (nil, reset?): return Text("\(symbol) \(reset)")
        case let (lead?, nil): return Text(lead)
        case (nil, nil): return nil
        }
    }
}

/// One ring per pool, nested like Activity rings in the same 22-point column as a single ring: the outer ring
/// is the first pool given, each further pool sits inside it.
struct PoolRings: View {
    let rings: [(pool: String?, window: QuotaReport.Window?)], drawing: Bool
    static let size: CGFloat = 22

    var body: some View {
        let count = CGFloat(rings.count), gap: CGFloat = 1
        let width = rings.count == 1 ? Self.size / 8 : min(3, (Self.size / 2 - 2) / count - gap)
        ZStack {
            ForEach(rings.indices, id: \.self) { index in
                Ring(value: value(index), state: state(index), size: Self.size - 2 * CGFloat(index) * (width + gap), lineWidth: width)
            }
        }
        .frame(width: Self.size, height: Self.size)
        .help(rings.count > 1 ? rings.map { "\($0.window?.poolLabel ?? $0.pool.map(Fmt.human) ?? "") models \($0.window?.usedPercent.map(Fmt.percent) ?? "–")" }
            .joined(separator: " · ") : "")
    }

    private func value(_ index: Int) -> Double? { rings[index].window?.usedPercent }
    private func state(_ index: Int) -> String { drawing ? "credit_only" : rings[index].window?.pace.state ?? "unknown" }
}

struct WindowList: View {
    let account: QuotaReport.Account, now: Date

    var body: some View {
        let meta = [account.planTitle,
                    account.fingerprint.map { "identity \($0)" },
                    account.priority.map { "priority \($0.formatted(.number.precision(.fractionLength(0))))" },
                    account.note,
                    account.requiresAuth ? "authentication required" : nil,
                    account.observedAt.map { "observed \(Fmt.ago($0, now: now))" },
                    account.status == .stale ? (account.error != nil ? "last fetch failed" : "window reset since reading") : nil].compactMap { $0 }
        let burn = account.isDrawing(now: now) ? account.creditBurnPerHour : nil
        // Present only when the account has some.
        let credit = account.creditBalance.map { ["Credit balance: \($0)"] } ?? []
        let resets = account.banked.map { [Fmt.resets($0)] } ?? []
        let extras = account.credits.map { Fmt.credit($0, burn: burn) } + credit + resets
        VStack(alignment: .leading, spacing: 6) {
            // A window the provider did not report is left out: missing never means 0%.
            ForEach(account.windows.filter { $0.usedPercent != nil }) { window in
                WindowLine(window: window, now: now)
            }
            ForEach(extras, id: \.self) { Text($0).lineLimit(2) }
            if !meta.isEmpty { Text(Fmt.sentence(meta.joined(separator: " · "))).foregroundStyle(.tertiary) }
        }
        .font(.subheadline).foregroundStyle(.secondary)
    }
}

/// "Claude models · Weekly" and its use, with the reset as a fact beneath and any estimate below that.
struct WindowLine: View {
    let window: QuotaReport.Window, now: Date

    var body: some View {
        let expired = window.isExpired(now)
        let fact = expired ? window.resetAt.map { "Reset since \(Fmt.moment($0, now: now))" }
            : window.pace.state == "exhausted" ? "Exhausted · " + Fmt.back(window.resetAt, now: now)
            : Fmt.resets(window.resetAt, now: now).map(Fmt.sentence)
        let estimate = expired ? nil : Fmt.projection(window, now: now)
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Ring(value: expired ? nil : window.usedPercent, state: expired ? "unknown" : window.pace.state, size: 10)
                .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
            VStack(alignment: .leading, spacing: 1) {
                Text(window.fullTitle).foregroundStyle(.primary).lineLimit(window.limitAmount == nil ? 1 : 2)
                if let fact { Text(fact) }
                if let estimate { Text(estimate).foregroundStyle(.tertiary) }
            }
            Spacer(minLength: 4)
            Text(expired ? "–" : Fmt.used(window)).monospacedDigit()
        }
        .help(window.resetAt.map { (expired ? "Reset " : "Resets ") + Fmt.exact($0) } ?? "Reset time not reported")
        .accessibilityElement(children: .combine)
    }
}

/// A capacity ring. Colour is a small tint that carries pace meaning, over a faint track of the same tint, as in
/// Activity rings; system colours adapt to light and dark. Increase Contrast strengthens the track.
struct Ring: View {
    let value: Double?, state: String, size: CGFloat
    var lineWidth: CGFloat? = nil
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        let width = lineWidth ?? max(size / 8, 2), tint = Self.tint(state)
        ZStack {
            Circle().inset(by: width / 2).stroke(tint.opacity(contrast == .increased ? 0.4 : 0.2), lineWidth: width)
            Circle().inset(by: width / 2).trim(from: 0, to: min(max((value ?? 0) / 100, 0), 1))
                .stroke(tint, style: StrokeStyle(lineWidth: width, lineCap: .round)).rotationEffect(.degrees(-90))
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    static func tint(_ state: String) -> Color {
        ["exhausted": .red, "credit_only": .blue, "ahead_of_pace": .orange, "deficit": .orange, "on_pace": .green, "reserve": .green, "early": .green, "not_started": .green][state]
            ?? .secondary
    }
}

/// A menu item: full-width target, the rounded hover highlight macOS menus use, and its shortcut shown trailing.
struct MenuRow<Label: View>: View {
    var shortcut: KeyEquivalent? = nil
    let action: () -> Void
    @ViewBuilder let label: Label
    @State private var hovering = false
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        Button(action: action) {
            HStack {
                label
                Spacer(minLength: 0)
                if let shortcut { Text("⌘\(String(shortcut.character).uppercased())").foregroundStyle(.secondary).accessibilityHidden(true) }
            }
            .padding(.horizontal, 8).padding(.vertical, 4).contentShape(Rectangle())
            .background(.quaternary.opacity(hovering && isEnabled ? 1 : 0), in: RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .keyboardShortcut(shortcut.map { KeyboardShortcut($0) })
        .onHover { hovering = $0 }
    }
}
