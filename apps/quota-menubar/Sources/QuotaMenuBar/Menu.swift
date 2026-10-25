import Foundation

/// What choosing a menu item does.
enum MenuAction: Equatable {
    case refresh, quit
    case open(URL)
}

/// A system colour for an item's symbol. Each maps to a semantic `NSColor`, so it adapts to light, dark and
/// Increase Contrast.
enum MenuTint: Equatable { case green, orange, red, blue, secondary }

/// One entry of the quota menu. The same list builds the `NSMenu` (StatusMenu.swift) and the `--render` preview
/// (Render.swift), so a rendered PNG checks exactly what the menu shows.
enum MenuEntry: Equatable {
    case header(String)
    case item(MenuItemSpec)
    case separator
}

/// A standard menu item: title, optional subtitle beneath, a trailing badge, an SF Symbol, and a ⌘ shortcut.
struct MenuItemSpec: Equatable {
    var title: String
    var subtitle: String? = nil
    var badge: String? = nil   // trailing text, such as "84%"
    var symbol: String? = nil
    var tint: MenuTint? = nil
    var shortcut: String? = nil   // a key used with ⌘
    var enabled = true
    var action: MenuAction? = nil
    var submenu: [MenuEntry]? = nil
    var help: String? = nil
    var id: String? = nil   // an account id, for `--render --expand`
}

/// Builds the menu from the model's data. Presentation only: what an account's state means stays in Report.swift.
enum QuotaMenu {
    static let attentionLimit = 5
    /// A reading older than this shows its age on its row.
    static let agedAfter: TimeInterval = 1800

    @MainActor
    static func entries(for model: QuotaModel, now: Date) -> [MenuEntry] {
        entries(report: model.report, error: model.error, updatedAt: model.updatedAt,
                refreshing: model.isRefreshing || model.report?.isRefreshing(now: now) == true,
                lanes: LaneBoard.attention(model.lanes, now: now), stateRoot: model.stateRoot, now: now)
    }

    static func entries(report: QuotaReport?, error: String?, updatedAt: Date?, refreshing: Bool,
                        lanes: [AttentionLane], stateRoot: URL, now: Date) -> [MenuEntry] {
        var entries = notices(report: report, error: error)
        if let report {
            let sorted = report.sorted(now: now)
            var adapters: [String] = []
            for account in sorted where !adapters.contains(account.provider) { adapters.append(account.provider) }
            for adapter in adapters {
                let members = sorted.filter { $0.provider == adapter }
                section(&entries, members[0].providerName ?? Fmt.human(adapter),
                        members.map { .item(account($0, now: now, alert: report.alertPercent)) })
            }
        }
        if !lanes.isEmpty {
            var items: [MenuEntry] = lanes.prefix(attentionLimit).map { item in
                .item(MenuItemSpec(title: item.lane.route, subtitle: "\(item.kind.title) · \(Fmt.ago(item.since, now: now))",
                                   symbol: item.kind.symbol, tint: tint(item.kind),
                                   action: .open(LaneBoard.runDirectory(item.lane, stateRoot: stateRoot)),
                                   help: "Open the run directory"))
            }
            if lanes.count > attentionLimit {
                items.append(.item(MenuItemSpec(title: "\(lanes.count - attentionLimit) more", enabled: false)))
            }
            section(&entries, "Needs attention", items)
        }
        let updated = updatedAt.map { "Updated \(Fmt.ago($0, now: now))" } ?? "Not updated yet"
        let status = refreshing ? "Refreshing…"
            : ([updated] + [report?.nextCheck.map { Fmt.next($0, now: now) }].compactMap { $0 }).joined(separator: " · ")
        if !entries.isEmpty { entries.append(.separator) }
        entries += [
            .item(MenuItemSpec(title: status, enabled: false, help: report?.nextCheck.map { Fmt.nextInFull($0, now: now) })),
            .item(MenuItemSpec(title: "Refresh", shortcut: "r", enabled: !refreshing, action: .refresh)),
            .item(MenuItemSpec(title: "Open Report Folder", enabled: report != nil,
                               action: report.map { .open(URL(fileURLWithPath: $0.reportPath).deletingLastPathComponent()) })),
            .separator,
            .item(MenuItemSpec(title: "Quit Quota", shortcut: "q", action: .quit)),
        ]
        return entries
    }

    /// A header over its items, after a separator unless it opens the menu.
    private static func section(_ entries: inout [MenuEntry], _ title: String, _ items: [MenuEntry]) {
        if !entries.isEmpty { entries.append(.separator) }
        entries.append(.header(title))
        entries += items
    }

    /// A failed refresh, a report still loading, or a report with no account snapshots.
    private static func notices(report: QuotaReport?, error: String?) -> [MenuEntry] {
        let warning = "exclamationmark.triangle.fill"
        guard let report else {
            return [.item(error.map {
                MenuItemSpec(title: "Quota unavailable", subtitle: clip($0), symbol: warning, tint: .orange, enabled: false, help: $0)
            } ?? MenuItemSpec(title: "Loading…", enabled: false))]
        }
        var entries: [MenuEntry] = []
        if let error {
            entries.append(.item(MenuItemSpec(title: "Last refresh failed", subtitle: "Showing the last report · " + clip(error),
                                              symbol: warning, tint: .orange, enabled: false, help: error)))
        }
        if report.accounts.isEmpty {
            entries.append(.item(MenuItemSpec(
                title: "No account snapshots",
                subtitle: "\(report.coverage.known) of \(report.coverage.total) quota report rows have current account data",
                enabled: false)))
            entries += report.sources.map { source in
                .item(MenuItemSpec(title: source.title, subtitle: source.error.map { clip($0) }, badge: source.statusTitle, enabled: false))
            }
        }
        return entries
    }

    /// One account: its label, its binding window's use and reset, its state when that is not plain availability,
    /// and the reading's age once it is old.
    static func account(_ account: QuotaReport.Account, now: Date, alert: Double) -> MenuItemSpec {
        let rank = account.rank(now), binding = account.binding(now: now)
        let shown = account.hasReading ? account.display(now: now, alert: alert) : nil
        let age = Self.age(account, now: now)
        var parts: [String?] = []
        var badge: String? = nil, symbol: String, tint: MenuTint
        switch rank {
        case 0:
            badge = shown.map { Fmt.percent($0.usedPercent ?? 0) }
            parts = shown.map { [$0.fullTitle, Fmt.resets($0.resetAt, now: now)] } ?? ["No current reading"]
            parts += account.spentPools(now: now).map { Fmt.spent($0, now: now) }
            let used = shown?.usedPercent ?? 0
            (symbol, tint) = (MenuLabel.gauge(used), used >= alert ? .orange : .green)
        case 1:
            let back = binding?.pace.state == "exhausted" ? binding?.resetAt.map { "allowance " + Fmt.back($0, now: now) } : nil
            parts = ["Drawing on credits", back ?? "allowance spent"]
            (symbol, tint) = ("creditcard.fill", .blue)
        case 3:
            parts = ["Exhausted", Fmt.back(account.back(now: now), now: now)]
            (symbol, tint) = (MenuLabel.gauge(100), .red)
        case 4:
            parts = [account.statusTitle]
            (symbol, tint) = ("person.crop.circle.badge.exclamationmark", .orange)
        default:
            // Stale, unknown or unsupported: the last reading, if any, keeps its figure but is marked by its state.
            badge = shown.map { Fmt.percent($0.usedPercent ?? 0) }
            parts = [account.statusTitle] + (shown.map { [$0.fullTitle, Fmt.resets($0.resetAt, now: now)] } ?? [account.error.map { clip($0) }])
            (symbol, tint) = (account.status == .stale ? "clock.badge.exclamationmark" : "questionmark.circle", .secondary)
        }
        if let age { parts.append(age) }
        return MenuItemSpec(title: account.displayName ?? "Default", subtitle: parts.compactMap { $0 }.joined(separator: " · "),
                            badge: badge, symbol: symbol, tint: tint, submenu: details(account, now: now),
                            help: account.id, id: account.id)
    }

    /// "7h ago" for a stale reading or one older than half an hour; nil while it is fresh.
    static func age(_ account: QuotaReport.Account, now: Date) -> String? {
        guard let observed = account.observedAt else { return nil }
        guard account.status == .stale || now.timeIntervalSince(observed) > agedAfter else { return nil }
        return Fmt.ago(observed, now: now)
    }

    /// An account's submenu: every reported window, then its credits and details, as information items.
    static func details(_ account: QuotaReport.Account, now: Date) -> [MenuEntry] {
        // A window the provider did not report is left out: missing never means 0%.
        let windows: [MenuEntry] = account.windows.filter { $0.usedPercent != nil }.map { window in
            let expired = window.isExpired(now)
            let fact = expired ? window.resetAt.map { "Reset since \(Fmt.moment($0, now: now))" }
                : window.pace.state == "exhausted" ? "Exhausted · " + Fmt.back(window.resetAt, now: now)
                : Fmt.resets(window.resetAt, now: now).map(Fmt.sentence)
            let estimate = expired ? nil : Fmt.projection(window, now: now)
            let used = window.usedPercent ?? 0
            return .item(MenuItemSpec(
                title: window.fullTitle, subtitle: [fact, estimate].compactMap { $0 }.joined(separator: " · "),
                badge: expired ? "–" : Fmt.used(window), symbol: MenuLabel.gauge(expired ? 0 : used),
                tint: expired ? .secondary : window.pace.state == "exhausted" ? .red : .secondary, enabled: false,
                help: window.resetAt.map { (expired ? "Reset " : "Resets ") + Fmt.exact($0) } ?? "Reset time not reported"))
        }
        let burn = account.isDrawing(now: now) ? account.creditBurnPerHour : nil
        let facts: [(String, String?)] = [
            ("Status", account.statusTitle),
            ("Plan", account.planTitle),
            ("Read", account.observedAt.map { Fmt.ago($0, now: now) }),
            ("Identity", account.fingerprint),
            ("Priority", account.priority.map { $0.formatted(.number.precision(.fractionLength(0))) }),
            ("Credit balance", account.creditBalance),
        ]
        var info: [MenuEntry] = facts.compactMap { key, value in
            value.map { .item(MenuItemSpec(title: key, badge: $0, enabled: false)) }
        }
        let lines = account.credits.map { Fmt.credit($0, burn: burn) } + (account.banked.map { [Fmt.resets($0)] } ?? [])
            + [account.note, account.error.map { "Error: " + clip($0) },
               account.status == .stale && account.error == nil ? "Window reset since reading" : nil]
                .compactMap { $0 }
        info += lines.map { .item(MenuItemSpec(title: $0, enabled: false)) }
        return (windows.isEmpty ? [] : [.header("Windows")] + windows + [.separator]) + [.header("Details")] + info
    }

    private static func tint(_ kind: Attention) -> MenuTint {
        switch kind {
        case .inputRequired: .blue
        case .failed: .red
        case .stalled: .orange
        }
    }

    /// A menu grows to its widest item, so free text is kept to one short line; the full text is in the tooltip.
    static func clip(_ text: String, _ limit: Int = 60) -> String {
        let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.count > limit ? line.prefix(limit - 1) + "…" : line
    }
}
