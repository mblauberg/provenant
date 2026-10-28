# Quota menu bar

An optional macOS menu-bar app that displays account quota snapshots from Provenant. It does not install or start itself.

The status item shows the highest-use available account and its percentage, beside the Provenant mark while usage is below the alert threshold and a gauge or warning symbol above it. Clicking it opens a standard macOS menu, with system highlighting, keyboard navigation, VoiceOver labels and light and dark appearance. The menu keeps the last good report visible if a refresh fails.

## Menu design

The menu follows the first-party menu extras of macOS 26, such as Wi-Fi and Battery: system fonts and metrics, one circular icon per row, colour only for state, one figure per row, and short secondary text.

- **Title row.** "Quota" and how fresh the data is: "Updated 2m ago", or "Refreshing…" while a refresh runs.
- **Accounts.** One row per account, kept together by provider in the order of each provider's most pressing account. The title is the provider's name, followed in the secondary colour by the account's label unless the label only repeats the provider. The trailing figure is the binding window's use, and the figures of every row end in one column beside the submenu arrows. The subtitle says what limits the account, such as "Weekly · resets Sun 1 Nov".
- **Icons.** A use ring shows the binding window's use: green below the alert threshold, orange above it. A full red ring is a spent allowance, whose subtitle says when it is back; a full blue ring is an account drawing on credits ("On credits until Sat 3:41 am"). Neither shows a figure, since the ring already does. A grey ring is a stale or unknown reading, whose dimmed figure is the last reading and whose subtitle says when it was checked. An orange ring around "!" asks for sign-in. A fresh reading carries no age; one older than 30 minutes adds "checked 2h ago".
- **Submenu.** Each account's submenu opens level with its row. It lists every reported window with its use bar and exact reset time, then the plan, credits, banked resets, when the reading was taken, priority and identity, then any note or error. It does not repeat the row's state, and no row with a submenu has a tooltip, which would cover the submenu.
- **Needs attention.** Lanes that need attention, each with its kind and how long ago it began.
- **Commands.** Refresh, Open Report Folder and Quit Quota. ⌘R and ⌘Q work while the menu is open, but no shortcut column is drawn, as in first-party menu extras, so the figures stay beside the arrows.
- **Customise.** A submenu in the footer. "Row figure" chooses which window each account row leads with: Automatic (the longest window unless a shorter one is at the alert level), 5-hour (the shortest current window of the binding group) or Weekly (the longest). "Menu bar figure" chooses the account the status item follows, or Most constrained, which skips hidden accounts. "Show accounts" has one checkmark per registered account. "Edit Names and Order…" opens the state directory's `config.toml`. The last line reminds you that `provenant accounts register --plan` adds accounts. Choices run `provenant quota display` and `provenant accounts set`, then reload `quota show`, so the menu follows at once.
- **Names, order and hiding.** An account's `display_name` replaces the title and its qualifier. Accounts with an `order` come first, by that number with ties kept in menu order, then the rest by provider. A hidden account is left out of the menu and the status item unless it is the chosen menu bar account. A per-account `headline` overrides the global row figure. Reports without these fields display as before.

Account rows are drawn by the app (`RowView.swift`) rather than as native titled items. A native item with a submenu reserves about 47 pt for its arrow, which pushed the figure column 12 pt too far left, and ignores the secondary colour of the qualifier. A drawn row ends its figure 34 pt from the right edge, draws its own chevron, and takes its highlight from the menu item, while the item still owns the submenu and VoiceOver reads each row as one menu item. `--render` draws the same view.

The menu's width depends only on its widest title or subtitle, with a minimum, so it does not shift as times and figures change. Rows without a snapshot show their reported status and error. When the CLI has no account snapshots, the menu shows how many quota report rows have known account data.

Binding limits must have a reset time and must not be advisory. Rust `credit_only` accounts have spent their plan allowance and appear as drawing on credits, with the allowance reset time when known.

The app reads all quota data from the CLI. It does not read credentials, tokens or the Keychain. The menu shows the configured account label, plan, usage windows, and available credit or reset details; an unavailable reading keeps its reported status and is never represented as zero.

Register each Claude account under a label in its required `~/.claude-LABEL` directory, for example `work` in `~/.claude-work`; see the [installation procedure](../../docs/runbooks/install.md#install). The CLI matches authenticated identities using fingerprints and selects the freshest unexpired matching credential from the registered directory or `~/.claude`. When needed, it asks Claude Code to refresh the registered directory's expired token. Raw email addresses are not stored.

The menu shows local requests and lanes needing attention from `provenant console overview --json`. It observes local database, WAL and project registry metadata as invalidation hints, coalesces changes for 250 ms, and reads the overview through the CLI. A change during a read queues another read; a failed read retains the previous overview and shows its age and error. A once-per-minute check recovers missed events or unavailable watches. Menu opening and manual refresh also request a read.

## Build and run

The app needs macOS 14 or later and the Swift toolchain. It has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
nohup .build/release/quota-menubar >/dev/null 2>&1 &
```

The app resolves `provenant` from the user's login shell and caches its executable path and `PATH`. Set `PROVENANT_BIN` to select an executable directly. Each CLI run happens off the main thread and has a 60-second limit.

At launch, the app runs `provenant quota show --json` to load the current report. It then runs `provenant quota tick --trigger <why> --power ac|battery|low --json` when the report is due, on ⌘R, after wake once the network is reachable, or when the power state changes. It watches the report file named by `report_path` and reloads it when a tick publishes a new report. Tick triggers are `timer`, `panel`, `manual`, `wake` and `power`.

A successful lane clears an authentication error for its account and publishes the changed report. On menu-open, wake and manual-refresh ticks, the CLI compares metadata for each registered provider's credential files and profile directory. A changed file or selected profile makes an authentication retry due immediately. On macOS, Claude also checks the default login Keychain file's metadata. Change detection never opens credential files or reads their contents, and adds no polling loop. Unchanged signed-out accounts retain the 15-minute authentication backoff; manual refresh can retry sooner.

The CLI command response is a versioned envelope containing the report. The watched file contains the raw report. The state root is `$PROVENANT_STATE_DIR`, defaulting to `~/.local/share/provenant/v1`; quota state and `report.json` live in its `quota` directory.

## Render the menu

`--render` draws the menu's content from a saved raw report into a PNG without running the CLI. AppKit draws a real menu only while it is open, so the PNG is a preview at macOS 26 menu metrics. It uses the same entries, attributed titles, icons and information views as the menu, and imitates only the native items' frame and the menu's glass background. `--expand <account id>` shows that account's submenu beside its highlighted row, and `--lanes` adds a saved `provenant lanes --json` page. The clock defaults to `generated_unix_seconds`; `--now` selects another instant. Rendering uses 2x scale unless `--scale` sets another factor.

```bash
.build/debug/quota-menubar --render Fixtures/current.json current.png --lanes Fixtures/lanes.json
.build/debug/quota-menubar --render Fixtures/current.json expanded.png --dark --expand codex/default
.build/debug/quota-menubar --render Fixtures/tall.json tall.png
```

The report fixtures use the same versioned raw report shape written by `quota tick`. `current.json` covers available, credit-only, exhausted, stale and authentication-required account rows, plus source-only statuses. `tall.json` holds 24 accounts; a menu taller than the screen scrolls as any macOS menu does. `lanes.json` holds one lane of each kind that needs attention.

## Start at login

The supported way to start the app at login is a LaunchAgent. Adding the bare executable to Login Items opens a Terminal window, because macOS treats it as a script. Save the following as `~/Library/LaunchAgents/dev.provenant.quota-menubar.plist`, replacing the executable path with the absolute path to your release build:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.provenant.quota-menubar</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/provenant/apps/quota-menubar/.build/release/quota-menubar</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
```

Load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.provenant.quota-menubar.plist`. To stop it, run `launchctl bootout gui/$(id -u)/dev.provenant.quota-menubar` and delete the file. With `KeepAlive` false, quitting from the menu keeps the app closed until the next login.
