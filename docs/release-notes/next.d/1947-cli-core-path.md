## Breaking changes

The CLI core path is `run`, `status`, `land`, `quota`, `install` and help.
`run` creates a new requested worktree, submits, waits up to 240 seconds and
prints a compact result tail. Duration budgets such as `--wait 30m` and
`--wait 3600s` are accepted. Timeout reports the attempt and exact wait command.
`status` shows active and recent lanes newest first; `--all` and `--json`
provide full records. `land N` replaces the separate PR merge path, verifies
CI at the current PR head and prunes after merge; `--head SHA` remains available.
Bare `quota` shows usage. `help --all` and `help COMMAND` reveal advanced tools
and flags.

CLI run, dispatch, cancel and resume generate and print operation IDs when
omitted; explicit IDs remain available for planned retries. Run and attempt
targets accept unique hex prefixes of at least eight characters and
`RUN/ATTEMPT`. Cancel and reconcile infer project scope from the run record.

## Fixes

Resuming a named route keeps the prior attempt's applied provider, model and
account instead of drawing the pool again. Refused continuation requests no
longer expose attempt IDs that were never recorded.
