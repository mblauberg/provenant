import Foundation

/// What choosing a menu item does.
enum MenuAction: Equatable {
    case refresh, quit
    case open(URL)
    /// Runs `provenant` with these arguments, then reloads `quota show`.
    case run([String])
}

/// A system colour that marks state. Each maps to a semantic `NSColor`, so it adapts to light, dark and
/// Increase Contrast. Colour is used for state only; everything else uses the menu's text colours.
enum MenuTint: Equatable, Hashable { case green, orange, red, blue, secondary }

/// The circular icon at the start of a row, one shape language for every row.
enum MenuIcon: Equatable, Hashable {
    /// A use ring: a faint track with an arc for the fraction used. A nil fraction draws the track alone.
    case ring(Double?, MenuTint)
    /// A full ring around a small SF Symbol, for a state that has no use figure.
    case mark(String, MenuTint)
    /// A plain monochrome SF Symbol, for commands.
    case symbol(String)
}

/// One entry of the quota menu. The same list builds the `NSMenu` (StatusMenu.swift) and the `--render` preview
/// (Render.swift), so a rendered PNG checks exactly what the menu shows.
enum MenuEntry: Equatable {
    /// The menu's title row and how fresh its data is.
    case title(String, String?)
    case header(String)
    case item(MenuItemSpec)
    /// A usage window in an account's submenu: name, figure, bar and reset.
    case window(WindowSpec)
    /// A labelled fact in an account's submenu.
    case fact(String, String)
    /// A line of text in an account's submenu, tinted when it reports a problem.
    case note(String, MenuTint?)
    case separator
}

/// A standard menu item: a title with optional secondary detail, a subtitle beneath, a trailing figure that lines
/// up across rows, a circular icon, and a ⌘ shortcut.
struct MenuItemSpec: Equatable {
    var title: String
    var detail: String? = nil     // after the title in the secondary colour, such as an account label
    var subtitle: String? = nil
    var value: String? = nil      // trailing figure, such as "84%"
    var dimValue = false          // a figure that may be out of date
    var icon: MenuIcon? = nil
    var shortcut: String? = nil   // a key used with ⌘
    var hidden = false            // carries only its shortcut, as first-party menu extras show none
    var enabled = true
    var checked = false           // a checkmark, for a choice among several
    var action: MenuAction? = nil
    var submenu: [MenuEntry]? = nil
    var help: String? = nil       // never set on a row with a submenu, where it would cover the submenu
    var id: String? = nil         // an account id, for `--render --expand`
    /// Account rows draw themselves (RowView.swift) so the figure column and the qualifier's colour are exact.
    var drawn: Bool { id != nil }
}

/// A usage window in an account's submenu.
struct WindowSpec: Equatable {
    var title: String
    var value: String
    var fraction: Double?   // nil once the window has reset since the reading
    var tint: MenuTint
    var caption: String?
}

/// Builds the menu from the model's data. Presentation only: what an account's state means stays in Report.swift.
enum QuotaMenu {
    static let title = "Quota"
    static let attentionLimit = 5
    /// A reading older than this shows when it was checked on its row.
    static let agedAfter: TimeInterval = 1800

    @MainActor
    static func entries(for model: QuotaModel, now: Date) -> [MenuEntry] {
        entries(report: model.report, error: model.error, updatedAt: model.updatedAt,
                refreshing: model.isRefreshing || model.report?.isRefreshing(now: now) == true,
                stateRoot: model.stateRoot, now: now, overview: model.overview,
                overviewError: model.overviewError, overviewUpdatedAt: model.overviewUpdatedAt,
                observationError: model.overviewObservationError)
    }

    static func entries(report: QuotaReport?, error: String?, updatedAt: Date?, refreshing: Bool,
                        stateRoot: URL, now: Date, overview: ConsoleOverview? = nil,
                        overviewError: String? = nil, overviewUpdatedAt: Date? = nil,
                        observationError: String? = nil) -> [MenuEntry] {
        let freshness = refreshing ? "Refreshing…" : updatedAt.map { "Updated \(Fmt.ago($0, now: now))" }
        var entries: [MenuEntry] = [.title(title, freshness)]
        entries += overviewEntries(overview, error: overviewError, updatedAt: overviewUpdatedAt, now: now,
                                   observationError: observationError)
        entries += notices(report: report, error: error)
        if let report, !report.accounts.isEmpty {
            let listed = report.listed(now: now)
            let available = listed.filter { $0.rank(now) != 3 }
            let spent = listed.filter { $0.rank(now) == 3 }
            if !listed.isEmpty { entries.append(.separator) }
            entries += available.map { .item(account($0, now: now, alert: report.alertPercent, headline: report.leading($0))) }
            if !spent.isEmpty {
                entries.append(.item(MenuItemSpec(title: "Spent accounts", value: "\(spent.count)",
                    icon: .ring(1, .red), submenu: spent.map {
                        .item(account($0, now: now, alert: report.alertPercent, headline: report.leading($0)))
                    })))
            }
        }
        entries += [
            .separator,
            .item(MenuItemSpec(title: "Refresh", icon: .symbol("arrow.clockwise"), enabled: !refreshing, action: .refresh)),
            .item(MenuItemSpec(title: "Open Console", subtitle: "Console is not installed yet · use provenant inbox",
                               icon: .symbol("macwindow"), enabled: false)),
            .item(MenuItemSpec(title: "Settings", icon: .symbol("slider.horizontal.3"), submenu:
                (report.map { customise($0, stateRoot: stateRoot) } ?? []) + [
                    .item(MenuItemSpec(title: "Open Report Folder", enabled: report != nil,
                        action: report.map { .open(URL(fileURLWithPath: $0.reportPath).deletingLastPathComponent()) })),
                    .item(MenuItemSpec(title: "Quit Quota", action: .quit)),
                ])),
            .item(MenuItemSpec(title: "Refresh", shortcut: "r", hidden: true, enabled: !refreshing, action: .refresh)),
            .item(MenuItemSpec(title: "Quit Quota", shortcut: "q", hidden: true, action: .quit)),
        ]
        return entries
    }

    static func overviewEntries(_ overview: ConsoleOverview?, error: String?, updatedAt: Date?, now: Date,
                                observationError: String? = nil) -> [MenuEntry] {
        var entries: [MenuEntry] = []
        if let overview {
            let count: String
            switch (overview.coverage.inbox, overview.counts.needsYou) {
            case ("complete", .some(let value)): count = "\(value) needs you"
            case ("partial", .some(let value)): count = "At least \(value) needs you"
            default: count = "Needs you unavailable"
            }
            entries += [.separator, .header(count)]
            entries += overview.needsYou.prefix(3).map { request in
                let context = [request.projectLabel, request.taskLabel].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
                return .item(MenuItemSpec(title: clip(request.title ?? "Untitled request"), subtitle: context.isEmpty ? nil : clip(context),
                                          icon: .mark("questionmark", .blue), enabled: false))
            }
            if overview.needsYou.count > 3 {
                entries.append(.note("\(overview.needsYou.count - 3) more · use provenant inbox", nil))
            }
            if overview.coverage.runs != "complete" { entries.append(.note("Local runs: \(overview.coverage.runs)", .orange)) }
            if overview.coverage.inbox != "complete" { entries.append(.note("Local inbox: \(overview.coverage.inbox)", .orange)) }
            if !overview.lanes.isEmpty {
                let running = overview.counts.running.map(String.init) ?? "?"
                let attention = overview.counts.attention.map(String.init) ?? "?"
                let prefix = overview.coverage.runs == "partial" ? "At least " : ""
                entries.append(.header("\(prefix)\(running) running · \(attention) attention"))
                entries += overview.lanes.prefix(5).map { lane in
                    let title = lane.taskLabel.flatMap { $0.isEmpty ? nil : $0 } ?? "Untitled task"
                    let state = lane.attention ? "Needs attention" : Fmt.human(lane.state)
                    let context = [lane.projectLabel, state].compactMap { $0 }.joined(separator: " · ")
                    return .item(MenuItemSpec(title: clip(title), subtitle: clip(context),
                                              icon: .mark(lane.attention ? "exclamationmark" : "circle", lane.attention ? .orange : .secondary),
                                              enabled: false))
                }
                if overview.lanes.count > 5 { entries.append(.note("More local work available in the console", nil)) }
            }
            entries.append(.note("Local overview · nested and remote work unavailable", nil))
        }
        if error != nil {
            let age = updatedAt.map { " · last updated \(Fmt.ago($0, now: now))" } ?? ""
            entries += [.separator, .item(MenuItemSpec(title: "Overview unavailable", subtitle: "Couldn’t refresh\(age)",
                                                        icon: .mark("exclamationmark", .orange), enabled: false))]
        }
        if let observationError {
            entries += [.separator, .item(MenuItemSpec(title: "Overview observation limited", subtitle: clip(observationError),
                                                        icon: .mark("exclamationmark", .orange), enabled: false,
                                                        help: observationError))]
        }
        return entries
    }

    /// A failed refresh, a report still loading, or a report with no account snapshots.
    private static func notices(report: QuotaReport?, error: String?) -> [MenuEntry] {
        let warning = MenuIcon.mark("exclamationmark", .orange)
        guard let report else {
            return [.separator, .item(error.map {
                MenuItemSpec(title: "Quota unavailable", subtitle: clip($0), icon: warning, enabled: false, help: $0)
            } ?? MenuItemSpec(title: "Loading…", enabled: false))]
        }
        var entries: [MenuEntry] = []
        if let error {
            entries.append(.item(MenuItemSpec(title: "Refresh failed", subtitle: "Showing the last report · " + clip(error, 40),
                                              icon: warning, enabled: false, help: error)))
        }
        if report.accounts.isEmpty {
            entries.append(.item(MenuItemSpec(
                title: "No account snapshots",
                subtitle: "\(report.coverage.known) of \(report.coverage.total) report rows have account data",
                icon: .ring(nil, .secondary), enabled: false)))
            entries += report.sources.map { source in
                .item(MenuItemSpec(title: source.title, subtitle: source.error.map { clip(Fmt.sentence($0)) },
                                   value: source.statusTitle, dimValue: true, icon: .ring(nil, .secondary), enabled: false))
            }
        }
        return entries.isEmpty ? [] : [.separator] + entries
    }

    /// One account: provider and label, the binding window's use as the row's one figure, and a short line saying
    /// what limits it. The ring's colour carries the state, so the line never repeats it, and a spent allowance
    /// shows when it comes back rather than a figure the full ring already gives.
    static func account(_ account: QuotaReport.Account, now: Date, alert: Double,
                        headline: QuotaReport.Headline = .auto) -> MenuItemSpec {
        let binding = account.binding(now: now)
        let shown = account.hasReading ? account.display(now: now, alert: alert, headline: headline) : nil
        // A chosen name replaces the title and its qualifier.
        var row = MenuItemSpec(title: account.entry?.name ?? account.providerName ?? Fmt.human(account.provider),
                               detail: account.entry?.name == nil ? label(account) : nil,
                               submenu: details(account, now: now, alert: alert), id: account.id)
        var parts: [String?] = []
        switch account.rank(now) {
        case 0:
            if let shown {
                let used = shown.usedPercent ?? 0
                row.value = Fmt.percent(used)
                row.icon = .ring(used / 100, used >= alert ? .orange : .green)
                parts = [windowName(shown), Fmt.resets(shown.resetAt, now: now)]
                parts += account.spentPools(now: now).map { Fmt.spent($0, now: now) }
                parts.append(checked(account, now: now))
            } else {
                row.icon = .ring(nil, .secondary)
                parts = ["No current reading"]
            }
        case 1:
            row.icon = .ring(1, .blue)
            let until = binding?.pace.state == "exhausted" ? binding?.resetAt.flatMap { $0 > now ? $0 : nil } : nil
            parts = [until.map { "On credits until \(Fmt.moment($0, now: now))" } ?? "On credits"]
        case 3:
            row.icon = .ring(1, .red)
            parts = [Fmt.sentence(Fmt.back(account.back(now: now), now: now))]
        case 4:
            row.icon = .mark("exclamationmark", .orange)
            parts = ["Sign-in required"]
        default:
            // Stale, unknown or unsupported: the last reading keeps its figure, dimmed, beside when it was taken.
            if let shown {
                let used = shown.usedPercent ?? 0
                row.value = Fmt.percent(used)
                row.dimValue = true
                row.icon = .ring(used / 100, .secondary)
                let when = account.observedAt.map { "last checked \(Fmt.ago($0, now: now))" }
                parts = [windowName(shown), account.status == .stale ? when ?? "out of date" : account.status == .unknown && account.error != nil ? "couldn’t check" : account.statusTitle.lowercased()]
            } else {
                row.icon = .mark("questionmark", .secondary)
                parts = [account.status == .unknown && account.error != nil ? "Couldn’t check" : account.statusTitle]
            }
        }
        row.subtitle = parts.compactMap { $0 }.joined(separator: " · ")
        return row
    }

    /// The Customise submenu: how rows and the menu bar lead, which accounts show, and the config file.
    static func customise(_ report: QuotaReport, stateRoot: URL) -> [MenuEntry] {
        func display(_ args: [String], _ title: String, _ on: Bool) -> MenuEntry {
            .item(MenuItemSpec(title: title, checked: on, action: .run(["quota", "display"] + args)))
        }
        var entries: [MenuEntry] = [.header("Row figure")]
        entries += [("auto", "Automatic", QuotaReport.Headline.auto), ("5h", "5-hour", .fiveHour), ("weekly", "Weekly", .weekly)]
            .map { display(["--headline", $0.0], $0.1, report.display.headline == $0.2) }
        entries.append(.header("Menu bar figure"))
        let icon = report.display.icon
        entries.append(display(["--icon", QuotaReport.Display.mostConstrained], "Most constrained",
                               icon == QuotaReport.Display.mostConstrained))
        let registered = report.accounts.filter { $0.entry != nil }
        entries += registered.compactMap { account in
            account.entry.map { display(["--icon", $0.id], $0.name ?? account.title, icon == $0.id) }
        }
        if !registered.isEmpty {
            entries.append(.header("Show accounts"))
            entries += registered.compactMap { account in
                account.entry.map { entry in
                    .item(MenuItemSpec(title: entry.name ?? account.title, checked: !entry.hidden,
                                       action: .run(["accounts", "set", entry.id, "--hidden", entry.hidden ? "false" : "true"])))
                }
            }
        }
        entries += [
            .separator,
            .item(MenuItemSpec(title: "Edit Names and Order…", action: .open(stateRoot.appending(path: "config.toml")))),
            .separator,
            .note("Add accounts with provenant accounts register --plan", nil),
        ]
        return entries
    }

    /// The account's label beside its provider's name, left out when it only repeats the provider.
    static func label(_ account: QuotaReport.Account) -> String? {
        guard var label = account.displayName?.trimmingCharacters(in: .whitespaces), !label.isEmpty else { return nil }
        let provider = (account.providerName ?? account.provider).lowercased()
        if label.lowercased().hasPrefix(provider + " ") { label = String(label.dropFirst(provider.count + 1)) }
        return [provider, account.provider.lowercased()].contains(label.lowercased()) ? nil : Fmt.sentence(label)
    }

    /// "Gemini weekly" or "5-hour": a window's name as a short phrase.
    static func windowName(_ window: QuotaReport.Window) -> String {
        guard let pool = window.poolLabel else { return window.title }
        return "\(pool) \(window.title.prefix(1).lowercased() + window.title.dropFirst())"
    }

    /// "checked 7h ago" for a reading older than half an hour; nil while it is fresh.
    static func checked(_ account: QuotaReport.Account, now: Date) -> String? {
        guard let observed = account.observedAt, now.timeIntervalSince(observed) > agedAfter else { return nil }
        return "checked \(Fmt.ago(observed, now: now))"
    }

    /// An account's submenu: every reported window with its bar, then the details the row has no room for. The row
    /// already shows the state, so it is not repeated here.
    static func details(_ account: QuotaReport.Account, now: Date, alert: Double) -> [MenuEntry] {
        // A window the provider did not report is left out: missing never means 0%.
        let windows: [MenuEntry] = account.windows.filter { $0.usedPercent != nil }.map { window in
            let used = window.usedPercent ?? 0
            if window.isExpired(now) {
                return .window(WindowSpec(title: windowName(window), value: "–", fraction: nil, tint: .secondary,
                                          caption: window.resetAt.map { "Reset since \(Fmt.moment($0, now: now))" }))
            }
            let exhausted = window.pace.state == "exhausted"
            let reset = exhausted ? Fmt.sentence(Fmt.back(window.resetAt, now: now)) : resetsInFull(window.resetAt, now: now)
            let tint: MenuTint = account.status == .stale || !window.setsState ? .secondary
                : exhausted ? .red : used >= alert ? .orange : .green
            return .window(WindowSpec(title: windowName(window), value: Fmt.used(window), fraction: min(used, 100) / 100,
                                      tint: tint, caption: [reset, Fmt.projection(window, now: now)].compactMap { $0 }
                                          .joined(separator: " · ")))
        }
        let facts: [(String, String?)] = [
            ("Plan", account.planTitle),
            ("Credits", account.creditBalance.map(credits)),
            ("Banked resets", account.banked.map(banked)),
            ("Checked", account.observedAt.map { Fmt.ago($0, now: now) }),
            ("Priority", account.priority.map { $0.formatted(.number.precision(.fractionLength(0))) }),
            ("Identity", account.fingerprint),
        ]
        let info: [MenuEntry] = facts.compactMap { key, value in value.map { .fact(key, $0) } }
        let notes: [MenuEntry] = [
            account.note.map { .note(clip($0), nil) },
            account.error.map { .note(clip(Fmt.sentence($0)), .orange) },
            account.status == .stale && account.error == nil ? .note("Window reset since reading", nil) : nil,
        ].compactMap { $0 }
        return [windows, info, notes].filter { !$0.isEmpty }.reduce([]) { $0.isEmpty ? $1 : $0 + [.separator] + $1 }
    }

    /// "Resets in 3h 48m" within a day, otherwise the exact time, which the row has no room for.
    static func resetsInFull(_ date: Date?, now: Date) -> String {
        guard let date else { return "Reset time not reported" }
        guard date > now else { return "Reset since \(Fmt.moment(date, now: now))" }
        if date.timeIntervalSince(now) < 86_400, let remaining = Fmt.until(date, now: now) { return "Resets in \(remaining)" }
        return "Resets \(Fmt.date(date)), \(date.formatted(date: .omitted, time: .shortened))"
    }

    /// "16,569" from the provider's decimal string; the string itself if it is not a number.
    static func credits(_ balance: String) -> String {
        guard let value = Double(balance) else { return balance }
        return value >= 100 ? value.formatted(.number.precision(.fractionLength(0)))
            : value.formatted(.number.precision(.fractionLength(0...2)))
    }

    /// "2 · soonest expires 3 Nov".
    static func banked(_ resets: QuotaReport.Resets) -> String {
        let lead = resets.expiresAt.count > 1 ? "soonest expires" : "expires"
        return ["\(resets.available)", resets.expiresAt.first.map { "\(lead) \(Fmt.day($0))" }].compactMap { $0 }
            .joined(separator: " · ")
    }

    /// A menu grows to its widest item, so free text is kept to one short line.
    static func clip(_ text: String, _ limit: Int = 60) -> String {
        let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.count > limit ? line.prefix(limit - 1) + "…" : line
    }
}
