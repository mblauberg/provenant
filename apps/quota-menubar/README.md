# Quota menu bar

An optional macOS menu-bar app that shows provider quota at a glance. Nothing installs or starts it by default.

Every two minutes it runs `provenant quota --json` and shows the highest used percentage across accounts in the menu bar, such as `Q 63%`. The menu lists each provider and account with its windows, how much of each is used, when each resets, and a marker when an account is in deficit or exhausted. It has Refresh (⌘R) and Quit (⌘Q) items.

The app never reads credential files, tokens or the Keychain. Everything it shows comes from the CLI's JSON report, schema `provenant.quota.report.v1`, which `tests/test_provider_quota.py` pins.

## Build and run

It needs macOS 14 or later and the Swift toolchain, and has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
.build/release/quota-menubar &
```

Two environment variables change its behaviour:

- `QUOTA_INTERVAL_SECONDS` sets the refresh interval, with a default of 120 and a minimum of 5.
- `PROVENANT_BIN` names the `provenant` executable. Without it, the app runs `provenant` through a zsh login shell so your normal `PATH` applies.

To start it at login, add the release binary to System Settings > General > Login Items, or write your own LaunchAgent that runs it.
