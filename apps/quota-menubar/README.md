# Quota menu bar

An optional macOS menu-bar app that shows provider quota at a glance. Nothing installs or starts it by default.

The menu bar shows a gauge symbol, which fills with the level, beside the highest used percentage, such as `63%`. That headline counts only active accounts that have a reading. It leaves out unavailable accounts, windows whose reset has already passed, and exhausted accounts, because an account that is exhausted for weeks would otherwise pin the headline at 100% and hide the provider you can still use. The headline reads 100% only when every active account is exhausted. A warning triangle replaces the gauge when the last refresh failed.

Clicking it opens a panel that groups accounts by provider. Each window has a bar coloured by its pace: green while on or under pace, orange when over pace, and red when exhausted. The panel also shows when the window resets or runs out, relative to now, and the panel keeps those times current while it is open. Hovering a window shows the reset in local time. The panel also shows the account's plan, how old the provider's reading is once it passes 30 minutes, and any account that cannot be read, with the provider's reason. If the CLI fails, the panel says why and keeps the last good report on screen, flagged as such. Refresh is ⌘R and Quit is ⌘Q. The app has no Dock icon.

The app never reads credential files, tokens or the Keychain. Everything it shows comes from the CLI's JSON report, schema `provenant.quota.report.v1`, which `tests/test_provider_quota.py` pins.

## Build and run

It needs macOS 14 or later and the Swift toolchain, and has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
.build/release/quota-menubar &
```

It runs `provenant quota --json` off the main thread, at most one run at a time, and stops the run after 60 seconds. One timer drives refreshes. It tolerates some delay so macOS can coalesce wakeups, stops while the Mac sleeps, and refreshes once on wake. Opening the panel on data older than half the interval also refreshes it.

Two environment variables change its behaviour:

- `QUOTA_INTERVAL_SECONDS` sets the refresh interval, with a default of 120 and a minimum of 5.
- `PROVENANT_BIN` names the `provenant` executable. Without it, the app runs `provenant` through a zsh login shell so your normal `PATH` applies.

To start it at login, add the release binary to System Settings > General > Login Items, or write your own LaunchAgent that runs it.
