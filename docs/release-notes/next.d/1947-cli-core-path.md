## Breaking changes
### CLI and install

- **The CLI is organised around a core path of `run`, `status`, `land`, `quota` and `install`.** `help --all` and `help COMMAND` show the advanced commands and flags (#1947).
- **`run` waits for its result.** It creates the requested new worktree, submits, waits up to 240 seconds and prints a compact result tail. On timeout it reports the attempt and the exact wait command. `--wait` accepts durations such as `30m` and `3600s` (#1947).
- **`status` is the overview of active and recent lanes, newest first.** `--all` and `--json` give full records, and bare `quota` shows usage (#1947).
- **`land N` replaces the separate PR merge command.** It checks CI at the current PR head and prunes after the merge. `--head SHA` is still available (#1947).
- **`run`, `dispatch`, `cancel` and `resume` print a generated operation ID when none is given.** Pass one explicitly for planned retries. Run and attempt targets accept a unique hex prefix of at least eight characters, or `RUN/ATTEMPT`, and `cancel` and `reconcile` infer project scope from the run record (#1947).
