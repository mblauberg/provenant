## Breaking changes
### CLI and install

- **Installing 2.0.0 resets format-1 state and needs your acknowledgement.** The install is blocked with `STATE_FORMAT_BREAK` until you pass `--reset-state` (`--yes` does not count), and while a lane process is live. It deletes the run store and run files, attempt tmp, graph journals, mail, claims, the host Fabric index with its chair bindings, the work index and PR contributions. The old usage ledger is archived to `usage-history/format-1/` so format 2 starts a fresh ledger. Provider history is relocated to `provider-history/format-1/`. Existing archive destinations refuse during preflight. Quota history, session checkpoints, chair log, worktrees and branches are untouched. Copy the state directory first if you want an archive. (#3163)

- Failed installs list only current agents actually stopped and print a rerunnable installer command. Empty or never-written SQLite stores do not block installation; a format-1 receipt also covers interrupted 1.x schema migrations. (#3078)
