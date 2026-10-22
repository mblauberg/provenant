# Quota menu bar

An optional macOS menu-bar app that shows provider quota at a glance. Nothing installs or starts it by default.

The menu bar shows a gauge symbol, which fills with the level, beside the highest used percentage, such as `63%`. The headline covers every enabled account that has a current reading, whether or not it is the account a provider is using now. It leaves out unavailable accounts, windows whose reset has already passed, and exhausted accounts. An account counts as exhausted only while one of its exhausted windows still has a reset ahead or an unknown reset, so an account that is out for weeks does not pin the headline at 100% and hide the accounts you can still use. The headline reads 100% only when no account outside exhaustion has a current reading. A warning triangle replaces the gauge when the last refresh failed.

Clicking it opens a panel in the style of the Wi-Fi and Battery menus. A summary ring at the top shows the headline percentage, the account it comes from, and that account's binding window, such as "Primary · runs out in 23h". Below it, each account is one menu-style row with a small capacity ring, a secondary line that names the binding window and when it resets or runs out, and its percentage. The binding window is the exhausted window that resets last for an exhausted account, and otherwise the current window with the highest use. Rows sit under quiet section headers, "Available", "Drawing on credits", "Unavailable" and "Exhausted", ordered by priority. Exhausted and unreadable rows are dimmed, and an exhausted row says when the account is back, such as "Exhausted · back in 3d 2h". An account the report marks `drawing_credits` has spent its allowance but stays usable on credits; its row says so, such as "Drawing on credits · allowance back in 2h", until the exhausted window behind it resets. Colour appears only as a small tint on each ring: green while on or under pace, orange when over pace, blue while drawing on credits, red when exhausted, and grey when the pace is unknown. A clock marks a reading that is stale or more than 30 minutes old.

Clicking a row discloses every window for that account, with its reset time on hover. A budget window reads in its currency, such as "Cloud session credits US$136 of US$250", and never binds the account, as in the CLI. When the account has them, the list adds a line per credit balance, with its expiry and, while drawing on credits, the burn rate, and a line for banked resets with the count and soonest expiry. It also shows how many windows the provider did not report, because a missing window never means 0%, and the plan and the reading's age. Relative times stay current while the panel is open. If the CLI fails, the panel says why and keeps the last good report on screen, flagged as such. A tall report scrolls. The footer shows when the report was last updated and, from the account due soonest, when the next check is and why, such as "Next check in 4m · active", or "Refreshing…" while a fetch runs. Menu-style Refresh (⌘R) and Quit (⌘Q) items follow. The app has no Dock icon.

The app never reads credential files, tokens or the Keychain. Everything it shows comes from the CLI's JSON report, schema `provenant.quota.report.v1`, which `tests/test_provider_quota.py` pins.

## Build and run

It needs macOS 14 or later and the Swift toolchain, and has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
nohup .build/release/quota-menubar >/dev/null 2>&1 &
```

On its first refresh the app asks a zsh login shell for the path to `provenant` and for your `PATH`, caches both, and from then on runs `provenant` directly with that `PATH`. If the cached path stops working, it resolves it again. Each run happens off the main thread, at most one at a time, and stops after 60 seconds.

The CLI decides what to fetch. The app runs `provenant quota tick --trigger <why> --power ac|battery|low --json`, and the CLI fetches only the accounts that are due and writes `report.json` as each one finishes. The app watches the quota state directory and reloads `report.json` when it changes, so a tick run by anything else shows up too. It runs a tick:

- at the report's earliest `next_due_at`, but no sooner than 30 seconds, from one timer that tolerates some delay so macOS can coalesce wakeups and stops while the Mac sleeps;
- when the panel opens (`panel`), or on ⌘R (`manual`), which fetches anything older than 30 seconds;
- after wake, once the network is reachable, with up to 15 seconds of jitter (`wake`);
- when the power source or Low Power Mode changes (`power`), which only recomputes the schedule.

The state directory is derived the way `scripts/provider_quota.py` derives it, `$AGENT_FABRIC_STATE_ROOT/quota`, by default `~/.local/state/agent-harness/fabric/quota`. The CLI inherits the app's environment, so both see the same root. If the directory does not exist yet, the app starts watching it after the first tick.

A CLI without `tick` exits with a usage error and no JSON. The app then keeps the original loop: `provenant quota --json` every interval, and again on opening the panel if the data is older than half the interval. ⌘R adds `--refresh`, so it bypasses the CLI's cache. After each wake the app checks for `tick` again.

Tick launches, failures and watcher re-arms go to the unified log under the app's bundle identifier, or `dev.provenant.quota-menubar` for a bare build, with no paths, output or account data. To follow them, run `log stream --level info --predicate 'subsystem == "dev.provenant.quota-menubar"'`.

Two environment variables change its behaviour:

- `QUOTA_INTERVAL_SECONDS` sets the interval for a CLI without `tick`, and the retry after a failed tick, with a default of 120 and a minimum of 5.
- `PROVENANT_BIN` names the `provenant` executable and skips the login-shell lookup.

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
