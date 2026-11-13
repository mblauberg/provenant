Provenant 2.0.0 changes the persisted run-state format. Before upgrading, drain
active lanes, land or park work in their worktrees, and copy the state directory
if you need an archive. Then run `provenant upgrade --reset-state` and
`provenant check install` in one sitting. The reset removes run records and
attempt state while keeping ledgers, provider history, session checkpoints and
chair logs. `--yes` does not acknowledge this reset.
