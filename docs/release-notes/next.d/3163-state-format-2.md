## Breaking changes
### CLI and install

- **Installing 2.0.0 resets format-1 state and needs your acknowledgement.** The install is blocked with `STATE_FORMAT_BREAK` until you pass `--reset-state` (`--yes` does not count), and while a lane process is live. It deletes the run store and run files, attempt tmp, graph journals, mail, claims, the host Fabric index with its chair bindings, the work index and PR contributions. The usage ledger, quota history, provider history, session checkpoints, chair log, worktrees and branches are untouched. Copy the state directory first if you want an archive. (#3163)
