## Fixes

- `provenant reconcile` and `provenant cancel` now settle writer attempts left
  running across a machine reboot as `lost` and release their reservations.
  Worktree contents and attempt telemetry are preserved for a resume (#1781).
