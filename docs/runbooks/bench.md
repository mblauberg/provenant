# Replay bench
Owner: provenant
Anchor: scripts/bench

`bench/tasks/*.toml` freezes small repository changes as replay tasks (parent
and merge commits, prompt, hidden test command, split and task family); the
adjacent `.patch` holds only the hidden tests, kept out of the task worktree
until the run ends. Families are `writer`, `fix`, `review` and `read-only`;
mined tasks start as `unclassified` until reviewed.

1. `scripts/bench mine [--limit N] [--family FAMILY]` proposes tasks from merge
   commits that changed Rust tests, each with `split = "search"`. Review the
   prompt (issue text only, never the tests or diff), command, split (`search`,
   `acceptance` or `held-out`) and family, and keep the patch beside the TOML.
2. `scripts/bench verify bench/tasks/TASK.toml` requires the tests to fail on
   the parent and pass on the merge, and writes a `.verified` receipt bound to
   the TOML and patch; editing either invalidates it.
3. `scripts/bench run --route R [--split S] [--limit N] [--repeats N]
   [--family FAMILY]` replays verified tasks in disposable worktrees and appends
   to `<state-root>/bench.jsonl`. The defaults (`--split acceptance`,
   `--repeats 1`, all families) are printed with the run summary. `--repeats N`
   launches each task N times in its own worktree and records one row per
   attempt with its `repeat` index; use it to average out launch noise. Each
   row carries the task family, the provider token categories from
   `telemetry stats` under `tokens` (`input`, `cache_read`, `cache_write`,
   `output`, `reasoning`) and wall time under `seconds`. A category stays `null`
   when the attempt did not complete, the provider reported no usage for it, or
   only part of it was reported; a measured zero is `0`. A `null` `pass` means
   the hidden test never ran (dispatch failed or the patch did not apply); it
   counts as neither pass nor failure.
4. `scripts/bench report [--ledger PATH] [--route R] [--repeats N]
   [--family FAMILY]` folds the ledger into a JSON report. It gives pass rate,
   token means and wall-time mean/min/max/stdev per family and per task; the
   per-task `seconds.stdev` is the variance across repeats. Filters are shown
   in the report. `scripts/bench replay --route R [run options]` runs the set
   and prints the report in one command, using the same defaults.

Tune and compare routes on `acceptance` tasks. Keep `held-out` tasks frozen: do
not read their failures to adjust a prompt or route, and run them once for the
final score. Compare two routes on the same tasks, split and families; attempts
nest in tasks, so do not pool them as independent samples. The report informs a
change but gates nothing: no workflow or merge check consumes it (#3447).
