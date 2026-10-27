GC can reclaim idle Git-ignored declared output from linked worktrees without
ending a lane. Nested repositories protect their containing output directory.
Explicit worktree removal allows that declared output and names
undeclared ignored blockers such as `.env`. Projects may opt into foreign
worktree collection with `gc.projects.<id>.foreign_worktrees = "merged-clean"`.

Writer admission tries a five-second pressure collection before refusing low
disk, then rechecks all output volumes. Pressure sweeps skip network fetches
and worktree deletion, honour `gc.schedule=false` and limit writer-admission
collection to the launching project. Pressure still prunes declared idle output, with a one-day floor below the
free-space floor. Scheduled collection now runs daily.

`retention.logs_days` defaults to 30. Large stdout/stderr captures of closed
runs, including child stores, expire with a 64 KiB tail retained. Disk reports
measure captures separately as `attempt_logs`.

Scheduled GC excludes stale, transient and non-Git project roots.
**Breaking JSON change:** `gc.data.unobserved_projects` is a count rather than
a list of paths. New transient invocation roots no longer create persistent
project registrations.

Large declared output no longer exhausts the idleness budget for explicit or
foreign removal. Foreign output age uses directory-entry mtimes.
