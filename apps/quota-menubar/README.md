# Quota menu bar and console

The optional macOS menu agent displays account quota and opens Provenant
Console. It reads through the CLI and does not read credentials or open the
provider stores. A failed refresh retains the last good content and reports
its age or error. The [human inbox runbook](../../docs/runbooks/human-inbox.md)
explains the request and answer workflow.

## Packages

`ProvenantKit` owns the CLI client and change observation; `ProvenantDesign`
contains shared presentation components. `QuotaMenuBar` and
`ProvenantConsole` are separate processes. Test support provides replay fixtures.
The console's column layout is owned by `ConsoleView`: nested split views in a
column can cause AppKit constraint loops.

## Build and run

The menu needs macOS 14 or later, the console macOS 26, and both need the Swift
toolchain. From the repository root:

```sh
cd apps/quota-menubar
swift build -c release
nohup .build/release/quota-menubar >/dev/null 2>&1 &
```

The app resolves the CLI from the login shell; `PROVENANT_BIN` selects an
executable directly. `./bundle` builds the console application and its nested
menu agent; Xcode is needed to compile the app icon. The menu opens the console
when it is installed. The console remains running when its window closes;
quit it with Command-Q.

## Test and render

Run `swift test` in this directory. Tests replay CLI output; refresh the
console fixtures when the CLI's console JSON changes. For visual inspection,
build the debug targets and render from fixtures without running the CLI:

```sh
swift build
.build/debug/provenant-console --render overview out.png --fixtures Tests/ProvenantTestSupport/Fixtures
.build/debug/quota-menubar --render Fixtures/current.json current.png
```

The menu render approximates native menu chrome. The observation benchmark and
its usage are in `Fixtures/bench/hook.py`; use disposable state when measuring
answer delivery.

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

The console is not a login item. It is opened from the menu agent's **Open Console**, `provenant console open`, or the Dock; it stays running after its window closes, and ⌘Q quits it. Unloading the menu agent's LaunchAgent does not stop a running console.
