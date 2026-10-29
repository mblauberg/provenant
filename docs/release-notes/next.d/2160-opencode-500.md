## Fixes
### Telemetry and providers

- OpenCode lanes no longer fail with `UnexpectedStatus: 500` before a session starts, because the lane now grants the server's `shell` state directory that Seatbelt denied. A failed attempt reports the server's own bounded cause for an HTTP 5xx instead of a bare status, and the server log is kept in `stderr.log` (#2160).
