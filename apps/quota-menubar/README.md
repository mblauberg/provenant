# Quota menu bar

An optional macOS menu-bar app that shows provider quota at a glance. Nothing installs or starts it by default.

The menu bar shows a gauge symbol, which fills with the level, beside one account's use, such as `11%`. The CLI picks the account with `quota.menubar.headline` and names it in the report's `headline`: by default the usable account you used most recently on this host, or, with no local activity, the usable account with the highest priority. `worst` picks the usable account with the highest use, `<provider>/<account>` pins one account, and `none` shows the gauge alone. When nothing usable remains, the menu bar shows the gauge alone, filled to the top. An account the CLI picked that becomes exhausted before the next report gives way to the highest use; a pinned one stays. A warning triangle replaces the gauge when the last refresh failed. With a CLI that writes no `headline`, the menu bar shows the highest use across usable accounts, as before.

Each account shows one window: the longest current window, such as Weekly, unless a shorter one has reached `quota.display.short_window_alert_percent` (default 90) or is exhausted. A window whose reset has passed never shows as current use. Names come from the CLI's display vocabulary: provider names such as "Claude" and "OpenCode", window names from their length, such as "5-hour", "Weekly" and "Monthly", plan names such as "Pro Lite", and the account's `display_name` from `telemetry.json`. A fingerprinted account without a display name shows the provider name alone; hovering a row shows its account id.

Clicking it opens a panel in the style of the Wi-Fi and Battery menus. The summary at the top shows the headline account, such as "Claude · Anese", with its use in a ring and its window and reset beside it, such as "Weekly · resets Wed 20 Mar". Below it, each account is one menu-style row: a capacity ring, the account's name, and its percentage on the right. Rows sit under quiet section headers, "Available", "Drawing on credits", "Unavailable", "Exhausted" and "Signed out", ordered by priority, so a row never repeats its state. Under a usable account's name, a reset symbol leads the reset: "4h 40m" within a day, otherwise "Tue 19 Mar". The row names its window only when a shorter one has taken the display, such as "5-hour". An exhausted row is one line, with when the account is back on the right, such as "back Mon 9 am". Exhausted, unreadable and signed-out rows are dimmed, less so with Increase Contrast. A projection is labelled as an estimate, "At this pace, empty by Fri", and shows in the summary and the expanded windows only when the window would run out before it resets. An account the report marks `drawing_credits` has spent its allowance but stays usable on credits; its row says when the allowance is back, such as "Allowance back in 2h", until the exhausted window behind it resets. Colour appears only as a tint on each ring, over a faint track of the same tint: green while on or under pace, orange when over pace, blue while drawing on credits, red when exhausted, and grey when the pace is unknown. A clock marks a reading that is stale or more than 30 minutes old. VoiceOver reads each row as one item with its use, reset or return time spelled out.

An account with pools, such as Antigravity's Gemini models and Claude models, shows its pools as nested rings, as in Activity, at the size of a single ring: the pool that binds the account outside, the others within. It stays usable while any pool is, and its row names the usable pool, such as "Claude models", with a line for each exhausted pool, such as "Gemini back Mon 9 am". Expanding the row lists each pool's windows with their use. When every pool is exhausted, the account is back when the first pool returns.

A signed-out account is one the CLI remembers after you switch a single-account sign-in, such as `agy`, to another account. Its row keeps the last reading and shows when it comes back, such as "back Mon 9 am", or "back since Wed 1 pm" once that time has passed; a ring empties once its window has reset. The CLI never fetches it. Right-click the row, or expand it, to forget it, which runs `scripts/provenant-quota forget <provider>/<account>`. Signing in again brings the account back with its history.

Clicking a row discloses every window for that account, each with its reset beneath and the exact time on hover. A budget window reads in its currency, such as "Cloud session credits US$30 of US$100", and never binds the account, as in the CLI. When the account has them, the list adds a line per credit balance, with its expiry and, while drawing on credits, the burn rate, and a line for banked resets with the count and soonest expiry. A window the provider did not report is left out, because a missing window never means 0%; the report's `unreported` still lists it. The plan and the reading's age close the list. Relative times stay current while the panel is open. If the CLI fails, the panel says why and keeps the last good report on screen, flagged as such. The panel hugs a short report; a tall one stops short of the bottom of the screen and its account list scrolls. The footer is one line: when the report was last updated and, from the account due soonest, when the next check is, such as "Updated just now · next in 4m", or "Refreshing…" while a fetch runs. Hovering it shows why, such as "Next check in 4m · active". Menu-style Refresh (⌘R) and Quit (⌘Q) items follow. The app has no Dock icon.

The status item redraws only when what it shows changes: on a report that changes it, or when a reset passes. The app keeps no repeating clock while the panel is closed.

The app never reads credential files, tokens or the Keychain. Everything it shows comes from the CLI's JSON report, schema `provenant.quota.report.v1`, which `tests/test_provider_quota.py` pins.

## Build and run

It needs macOS 14 or later and the Swift toolchain, and has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
nohup .build/release/quota-menubar >/dev/null 2>&1 &
```

On its first refresh the app asks your login shell (`$SHELL`, else the account's shell; zsh, bash and fish all work) for the path to `provenant` and for your `PATH`, caches both, and from then on runs `provenant` directly with that `PATH`. If the shell does not find it, the app uses `~/.local/bin/provenant` when that exists; otherwise the menu shows that `provenant` was not found. If the cached path stops working, it resolves it again. Each run happens off the main thread, at most one at a time, and stops after 60 seconds.

The CLI decides what to fetch. The app runs `scripts/provenant-quota tick --trigger <why> --power ac|battery|low --json`, and the CLI fetches only the accounts that are due and writes `report.json` as each one finishes. The app watches the quota state directory and reloads `report.json` when it changes, so a tick run by anything else shows up too. It runs a tick:

- at the report's earliest `next_due_at`, but no sooner than 30 seconds, from one timer that tolerates some delay so macOS can coalesce wakeups and stops while the Mac sleeps;
- when the panel opens (`panel`), or on ⌘R (`manual`), which fetches anything older than 30 seconds;
- after wake, once the network is reachable, with up to 15 seconds of jitter (`wake`);
- when the power source or Low Power Mode changes (`power`), which only recomputes the schedule.

The state directory is derived the way `scripts/provider_quota.py` derives it, `$AGENT_FABRIC_STATE_ROOT/quota`, by default `~/.local/state/agent-harness/fabric/quota`. The CLI inherits the app's environment, so both see the same root. If the directory does not exist yet, the app starts watching it after the first tick.

A CLI without `tick` exits with a usage error and no JSON. The app then keeps the original loop: `scripts/provenant-quota --json` every interval, and again on opening the panel if the data is older than half the interval. ⌘R adds `--refresh`, so it bypasses the CLI's cache. After each wake the app checks for `tick` again.

Tick launches, failures and watcher re-arms go to the unified log under the app's bundle identifier, or `dev.provenant.quota-menubar` for a bare build, with no paths, output or account data. To follow them, run `log stream --level info --predicate 'subsystem == "dev.provenant.quota-menubar"'`.

Two environment variables change its behaviour:

- `QUOTA_INTERVAL_SECONDS` sets the interval for a CLI without `tick`, and the retry after a failed tick, with a default of 120 and a minimum of 5.
- `PROVENANT_BIN` names the `provenant` executable and skips the login-shell lookup.

## Render the panel

`--render` draws the panel from a saved report into a PNG, offscreen, without running the CLI. The clock is the report's `generated_at` unless `--now` sets it. It draws at 2x, as on a Retina display, unless `--scale` sets another factor.

```bash
.build/debug/quota-menubar --render Fixtures/current.json current.png
.build/debug/quota-menubar --render Fixtures/current.json expanded.png --dark --expand claude/anese,agy/google-4e7d2a91c05b
.build/debug/quota-menubar --render Fixtures/tall.json tall.png --screen-height 760
```

`Fixtures/` holds reports that `Fixtures/make_fixtures.py` writes with `quota_display.report()` from synthetic readings: `current.json` has one account of each kind, `tall.json` scrolls, and `old.json` is the same report without the fields an older CLI leaves out, to check that it still decodes.

## Start at login

The supported way to start it at login is a LaunchAgent. Adding the bare executable to Login Items opens a Terminal window, because macOS treats it as a script. Save the following as `~/Library/LaunchAgents/dev.provenant.quota-menubar.plist`, and replace the path with the absolute path to your release build:

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

Load it now with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.provenant.quota-menubar.plist`. It will also start at each login. To stop it, run `launchctl bootout gui/$(id -u)/dev.provenant.quota-menubar` and delete the file. Because `KeepAlive` is false, quitting from the panel keeps the app closed until the next login.
