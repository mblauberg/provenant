## Fixes
### Telemetry and providers

- `claude --cloud` submissions now work on a real host: the owner runs the submit under a pseudo-terminal, which the CLI requires, and reads the session ID from the `Resume with: claude --teleport` line. A refused submission keeps the CLI's first error lines in the saved cloud job, so `status` and `output` show why instead of an empty `submitting` state.
