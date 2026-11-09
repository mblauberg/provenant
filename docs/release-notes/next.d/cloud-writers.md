## Behaviour changes
### Telemetry and providers

- Cloud jobs can now write. `attempt.cloud` works with `--mode worktree-write` and with delivery targets. The job pushes without force to a named branch (`work_branch`, the delivery branch or the lane branch) and may merge the base branch. The owner settles a writer only after it verifies the push on `origin`, and that branch and head become the lane's result. Hosted Claude may push its `claude/*` branch instead, and the record names the real branch.
- Changed tracked files no longer fail a cloud result, and Codex thread jobs no longer submit with `--read-only`.
- Hosted Claude reports through a `Provenant-Cloud-Result:` trailer commit, so a finished job settles at the next `reconcile`. An unfinished job past its deadline is reported unknown with its session URL. Claude submissions pass `--model` and `--effort`.
- A Codex thread turn settles on its final-answer message even while the service still reports the turn in progress.
- Status shows the requested and observed model, the named and pushed branch, the head and the session URL. A cloud result still does not replace required CI on the exact head.
- Old cloud records are not migrated; settle them with `provenant reconcile --abandon`.
