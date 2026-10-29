# Replay bench
Owner: provenant
Anchor: tools/front-door/src/bench.rs

The bench freezes small repository changes as replay tasks. Each file in
`bench/tasks/` names the repository, parent and merge commits, task prompt,
hidden test command and split. The adjacent `.patch` contains only the tests
to install after dispatch; it is kept out of the task worktree until the run
finishes.

## Mine and verify

Run `provenant bench mine [--limit N]` from the repository. It proposes tasks
from merge commits that changed Rust test files. Review the prompt, test
command and split, then keep the generated test patch beside the TOML.

Run `provenant bench verify bench/tasks/TASK.toml` before accepting a task.
Verification applies the hidden test patch to a disposable parent worktree,
requires the test command to fail there, then runs it on a disposable merge
worktree and requires success. It writes a content-bound `.verified` receipt
beside the task; editing the TOML or test patch invalidates that receipt.

## Replay

Run `provenant bench run --route R [--split acceptance] [--limit N]`. The
default split is `acceptance`; `held-out` tasks are excluded unless that split
is named explicitly. Each task starts from its parent in a disposable Git
worktree, receives the prompt through the existing worktree-write dispatch,
then gets its hidden tests copied in and run. Results append to
`<state-root>/bench.jsonl` with task, route, model, pass, reason, token count,
cost, turn count and elapsed seconds. A completed replay records `pass` as
`true` or `false` and a `null` reason. When dispatch fails or the hidden patch
does not apply, `pass` is `null` and `reason` says why the hidden test never
ran. Turns count recorded provider attempts. Unknown usage, cost or turns are
recorded as `null`.

## Acceptance rule

Only verified tasks enter a run. Use acceptance tasks to select and compare a
route. Keep held-out tasks frozen and do not run them during route selection;
use them once to report the final replay score. A pass is the hidden test
command succeeding after the task prompt is dispatched. A row with a `null`
pass is incomplete and does not count as a pass or a failure.

## Splits, leakage and isolation

- **Splits.** Each task names one split. Use `acceptance` tasks to tune and
  compare routes. `held-out` tasks stay frozen: do not read their failures to
  adjust a prompt or route, and run them once for the final score.
- **Leakage.** The hidden test patch never enters the task worktree before the
  run ends, and `verify` binds the receipt to the TOML and patch contents. The
  prompt is the issue text only; it must not quote the tests or the merge diff.
- **Isolation.** Every task runs in a disposable worktree cut from the parent
  commit, so no task sees another's changes or the live checkout. Dispatch uses
  the same route, access and confinement rules as any other worktree-write lane.
- **Pairing.** Compare two routes or harness changes on the same tasks and
  split. Attempts nest in tasks; do not pool them as independent samples.
