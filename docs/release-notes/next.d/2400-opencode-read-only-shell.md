## Fixes
### Providers

- Read-only OpenCode lanes can run shell commands again. Their inline `bash` permission is now `allow`, and they receive the same system exec roots, `PATH` entries and read-only Rust/Node toolchain setup as read-only Codex lanes. Writes stay denied by the outer Seatbelt profile, and `edit` remains denied ([#2400](https://github.com/mblauberg/provenant/issues/2400)).
