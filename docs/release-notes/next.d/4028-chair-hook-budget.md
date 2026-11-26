## Fixes

### Hooks and chair

- Chair hooks now stay inside one local budget derived from the installed timeout. `domains.hooks.timeout_secs` (default 10) replaces the fixed 5/5/10-second registrations for chair-start, chair-stop and chair-prompt, and its layer is shown in `provenant doctor`. Every hook gets 60% of that timeout as its single budget; a hook that reaches it warns with `HOOK_BUDGET_OVERRUN`, names the overrun and its setting source, records the event under the state root's `hooks/diagnostics/failures.jsonl`, and returns before the host kills it. Usage ingest no longer runs on the chair path: chair hooks queue a detached `telemetry ingest --incremental` worker that resumes durable transcript cursors instead of rescanning a 24-hour window. `provenant doctor` (and `provenant hosts doctor`) reports the installed timeout, its source and every hook overrun in the last 24 hours. (#4028)
