## Fixes
### Telemetry and providers

- Claude lanes with open network now enable WebFetch and WebSearch. Read-only Claude lanes get the same toolchain setup as Codex lanes (Cargo and detected Homebrew tools on `PATH`, public macOS TLS files, Command Line Tools selection). Network-off behaviour and credential restrictions are unchanged (#2000).
