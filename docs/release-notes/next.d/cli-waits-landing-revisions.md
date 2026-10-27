## Behaviour changes

`lanes --wait` now counts only lanes that were unsettled when the wait started
or have settled since (or since `--since`), so a settled older attempt no longer
ends a wait on a newer one. An attempt named by its exact ID still counts
whenever it settled, and a wait on runs whose lanes all settled earlier is
refused with the count. `lanes` takes `--session LABEL`, `--host` works with
`--wait`, and `lanes --watch` prints one line per state change for a background
shell or monitor. The wait starts at command entry, so a lane that settles while a
peer is unreachable still counts. Each peer read is capped by the time the wait
has left and none starts after its deadline. A peer wait keeps every running
lane tracked until it settles, even after newer dispatches push it off the
peer's newest 100, and `--since` selects by settlement time rather than queue
order, scanning up to 1000 lanes.

`pr land NUMBER --head SHA` waits for CI, merges at that head through the
existing merge path, then fetches, fast-forwards a clean local base, removes
the merged branch's worktree (after its declared build output) and deletes the
branch with `git branch -d`. Each follow-up step refuses and reports rather than
forcing, and cleanup only touches the pull request's own branch: a fork's head
leaves a same-named local branch alone, and a worktree holding commits the base
lacks is kept along with its branch. `pr open --from-attempt` builds the pull
request body from the attempt's receipts into the repository template, and
`pr status` prints one line per pull request.

Session checkpoints now keep the last 8 saves as numbered revisions, each with
its parent and content hash. `workflow session show --rev N`, `show
--revisions` and `workflow session diff N` read them as plain text.
`[retention] session_revisions` (1 to 64) sets the count, for `workflow session
handoff` saves as well as `session save`. `session diff` now shows a change that
is only a final newline, with a `\ No newline at end of file` note.
