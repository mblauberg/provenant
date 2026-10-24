# Quota menu bar

An optional macOS menu-bar app that displays account quota snapshots from Provenant. It does not install or start itself.

The status item shows the highest-use available account and its percentage. The panel lists accounts by status, shows reported usage windows and reset times, and keeps the last good report visible if a refresh fails. Rows without a snapshot show their reported status and error. When the CLI has no account snapshots, the panel shows how many quota report rows have known account data.

Binding limits must have a reset time and must not be advisory. Rust `credit_only` accounts have spent their plan allowance and appear as "Exhausted, drawing on credits", with the allowance reset time when known.

The app reads all quota data from the CLI. It does not read credentials, tokens or the Keychain. Quota probes stay disabled unless a provider has a qualifying activation recording and recorded runtime authority. Until then, the CLI reports the provider status without inventing an account identity.

## Build and run

The app needs macOS 14 or later and the Swift toolchain. It has no third-party dependencies.

```bash
cd apps/quota-menubar
swift build -c release
nohup .build/release/quota-menubar >/dev/null 2>&1 &
```

The app resolves `provenant` from the user's login shell and caches its executable path and `PATH`. Set `PROVENANT_BIN` to select an executable directly. Each CLI run happens off the main thread and has a 60-second limit.

At launch, the app runs `provenant quota show --json` to load the current report. It then runs `provenant quota tick --trigger <why> --power ac|battery|low --json` when the report is due, on ⌘R, after wake once the network is reachable, or when the power state changes. It watches the report file named by `report_path` and reloads it when a tick publishes a new report. Tick triggers are `timer`, `panel`, `manual`, `wake` and `power`.

The CLI command response is a versioned envelope containing the report. The watched file contains the raw report. The state root is `$PROVENANT_STATE_DIR`, defaulting to `~/.local/share/provenant/v1`; quota state and `report.json` live in its `quota` directory.

## Render the panel

`--render` draws the panel from a saved raw report into a PNG without running the CLI. The clock defaults to `generated_unix_seconds`; `--now` selects another instant. Rendering uses 2x scale unless `--scale` sets another factor.

```bash
.build/debug/quota-menubar --render Fixtures/current.json current.png
.build/debug/quota-menubar --render Fixtures/current.json expanded.png --dark --expand claude/anese
.build/debug/quota-menubar --render Fixtures/tall.json tall.png --screen-height 760
```

The fixtures use the same versioned raw report shape written by `quota tick`. `current.json` covers available, credit-only, exhausted, stale and authentication-required account rows, plus source-only statuses. `tall.json` fills a scrollable panel.

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

Load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.provenant.quota-menubar.plist`. To stop it, run `launchctl bootout gui/$(id -u)/dev.provenant.quota-menubar` and delete the file. With `KeepAlive` false, quitting from the panel keeps the app closed until the next login.
