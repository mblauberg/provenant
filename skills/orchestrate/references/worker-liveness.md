# Worker liveness

Run the read-only helper as one command:

```bash
python3 skills/orchestrate/scripts/worker_liveness.py
```

By default it reports one row per dispatched `codex exec` worker in the current
Git repository. Use `--repo PATH` to select another repository. Wrapper
ancestors are collapsed into the real worker row, and `--cd` or `-C` determines
the reported worktree instead of the wrapper's launch directory. Each row shows
PID, elapsed wall time, accumulated CPU time, the newest matching
`~/.codex/sessions/**/rollout-*.jsonl` mtime, worktree dirtiness, and worker
cwd. The `STALLED?` label is advisory. It never signals, kills, restarts, or
supervises a process.

The helper's `STALL_WINDOW_SECONDS` is the single source of truth for the
stall rule: once a worker has run for at least that window, it is stalled only
when accumulated CPU is below one CPU-second per window of elapsed time and
the matching session log has also been unchanged for at least that window.
If no matching session log exists, that absence cannot clear a stall already
indicated by elapsed time and CPU. At the start of a run, a 0-byte output file
is ambiguous. After a minute it is suspicious, but output size alone is not a
liveness test.

Detached harness-task state is not process state. Before reusing a worktree,
confirm the worker PID is gone. Otherwise the detached task and a fresh worker
can become two writers in the same worktree, as happened when detached tasks
continued running for hours.
