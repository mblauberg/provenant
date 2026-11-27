# Replay bench
Owner: provenant
Anchor: scripts/bench

`bench/tasks/*.toml` freezes small repository changes as replay tasks (parent
and merge commits, prompt, hidden test command, split); the adjacent `.patch`
holds only the hidden tests, kept out of the task worktree until the run ends.

1. `scripts/bench mine [--limit N]` proposes tasks from merge commits that
   changed Rust tests, each with `split = "search"`. Review the prompt (issue
   text only, never the tests or diff), command and split (`search`,
   `acceptance` or `held-out`), and keep the patch beside the TOML.
2. `scripts/bench verify bench/tasks/TASK.toml` requires the tests to fail on
   the parent and pass on the merge, and writes a `.verified` receipt bound to
   the TOML and patch; editing either invalidates it.
3. `scripts/bench run --route R [--split S] [--limit N] [--repeats N]` replays
   verified tasks of one split (default `acceptance`) in disposable worktrees
   and appends to `<state-root>/bench.jsonl`. `--repeats N` (default 1) launches
   each task N times in its own worktree and records one row per attempt with
   its `repeat` index; use it to average out launch noise. Each row carries the
   provider token categories from `telemetry stats` under `tokens` (`input`,
   `cache_read`, `cache_write`, `output`, `reasoning`), which stay `null` when
   the attempt did not complete. A `null` `pass` means the hidden test never ran
   (dispatch failed or the patch did not apply); it counts as neither pass nor
   failure.

Tune and compare routes on `acceptance` tasks. Keep `held-out` tasks frozen: do
not read their failures to adjust a prompt or route, and run them once for the
final score. Compare two routes on the same tasks and split; attempts nest in
tasks, so do not pool them as independent samples.
