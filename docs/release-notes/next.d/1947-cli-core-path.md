## Breaking changes

- **The CLI is organised around a core path of `run`, `status`, `land`, `quota` and `install`.** `help --all` and `help COMMAND` show the advanced commands and flags. The core commands change as follows.
- **`run` waits for its result.** It creates the requested new worktree, submits, waits up to 240 seconds and prints a compact result tail. It accepts duration budgets such as `--wait 30m` and `--wait 3600s`. On timeout it reports the attempt and the exact wait command.
- **`status` replaces the lane listing as the overview.** It shows active and recent lanes, newest first; `--all` and `--json` give full records. Bare `quota` shows usage.
- **`land N` replaces the separate PR merge command.** It checks CI at the current PR head and prunes after the merge. `--head SHA` is still available.
- **`run`, `dispatch`, `cancel` and `resume` generate and print an operation ID when none is given.** Pass one explicitly for planned retries. Run and attempt targets accept a unique hex prefix of at least eight characters, or `RUN/ATTEMPT`, and `cancel` and `reconcile` infer project scope from the run record.
