## Fixes

- OpenCode lanes no longer fail with `UnexpectedStatus: 500` before a session starts: the lane now grants the server's `shell` state directory, whose creation was denied under Seatbelt.
- OpenCode attempts keep the server log in `stderr.log`, and a failed attempt reports the server's own bounded cause for an HTTP 5xx instead of a bare status.
