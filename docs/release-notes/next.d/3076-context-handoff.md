## Behaviour changes
### Lanes and settlement

- A writer can hand off before its context grows costly: set `lanes.context_handoff_tokens` (suggested 200000) and the host stops a Claude writer once one call's context reaches it, settling the stop with the writer's latest progress handoff so the graph's restart continues it fresh on the same worktree with commits and edits kept. It is off by default (0) until a trial shows restart does not lower completion; providers that report no per-call context are unaffected. `provenant telemetry stats` now reports spend by context band (`context_bands` in `--json`) so windows before and after can be compared. (#3076)
