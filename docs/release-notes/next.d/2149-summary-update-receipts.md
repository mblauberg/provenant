## Behaviour changes

### CLI and install

`chair summary update` and the MCP `summary_update` operation take an optional `operation_id` (`--operation-id` on the CLI; generated when omitted). The checkpoint lock now journals the full publication plan and a receipt before any file changes, and every checkpoint writer replays a pending journal first. A retry with the same id and request returns the original result with `replayed: true`, even after a crash between publication steps. The same id with a different request is refused. The last 64 receipts per session are kept (#2149).
