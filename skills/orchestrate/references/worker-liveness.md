# Worker liveness

Run the read-only helper as one command:

```bash
python3 "${AGENTS_HOME:-$HOME/.agents}/skills/orchestrate/scripts/worker_liveness.py"
```

By default it reports one row per dispatched `codex exec` worker in the current
Git repository. Use `--repo PATH` to select another repository. Wrapper
ancestors are collapsed into the real worker row, and `--cd` or `-C` determines
the reported worktree instead of the wrapper's launch directory. Each row shows
PID, elapsed wall time, accumulated CPU time, the newest matching
`~/.codex/sessions/**/rollout-*.jsonl` mtime, worktree dirtiness, and worker
cwd. The `STALLED?` label is advisory. It never signals, kills, restarts, or
supervises a process.

The helper's `STALL_WINDOW_SECONDS` is the single advisory threshold. A live
worker with no observed output growth remains live and waiting; an unchanged
snapshot may prompt escalation, but it never proves failure or grants permission
to stop or reuse the worktree. CPU, session-log mtime, elapsed time and a
0-byte first snapshot are supporting observations only. Terminality comes from
the execution owner that waited for the worker and recorded its exit.

Detached harness-task state is not process state. Before reusing a worktree,
confirm the worker PID is gone. Otherwise the detached task and a fresh worker
can become two writers in the same worktree, as happened when detached tasks
continued running for hours.

## Native launcher ownership

The native launcher that starts a provider owns the wait, retry and process
lifecycle. A dispatching sub-agent returns only the launcher's observed
terminal result; it must not invent a second lifecycle around that process.

**Preferred path: let the native launcher wait for exit.** The provider command
owns its process and the caller records the result it returns:

```bash
codex exec -s <sandbox> -C <ABSOLUTE_DIR> -m <model> - < brief.txt > out.txt 2>&1
```

Give it the largest timeout the tool accepts. This is the whole procedure when
the run fits inside one timeout window.

If the provider turn exceeds a launcher timeout, use the existing native
launcher wait/retry contract and retain the attempt as non-terminal until the
execution owner observes exit. Do not replace that contract with another
process owner, and do not infer completion from an idle turn or unchanged
output.

## A dispatcher must actually dispatch

The second recurring failure is a dispatching sub-agent quietly doing the work
itself instead of invoking the external CLI. It looks like success: a real diff
lands, the report reads well, and nothing in the output says the worker never
ran.

It is not success. The delegation existed to buy something specific, and doing
the work in-agent destroys all of it: the caller's token budget is spent in the
expensive context instead of the cheap one, a cross-family review becomes a
same-family review, and a sub-agent chosen for being cheap produces work at a
capability tier nobody selected.

The temptation is strongest exactly where the guard matters most: when the CLI
is missing, unauthenticated, rate-limited, or fails its first invocation. That
is the moment to stop, not to substitute.

So:

- Never write the artefact yourself. Not the diff, not the review, not the
  analysis. Not even "just the easy part" while the worker handles the rest.
- If the CLI cannot be invoked or exits non-zero without producing output, that
  is the result. Report the failure, the exact command, and the captured stderr.
  A clean report of a failed dispatch is a good outcome; a silent substitution
  is a corrupted one.
- Never reconstruct what the worker "would have" concluded.

Make it checkable rather than trusting the intent. Every dispatch report carries
the exact command invoked, the worker's exit status, and the absolute path to a
non-empty transcript. A report without a transcript path did not dispatch.

Callers should verify the same way, because the claim is cheap to check: confirm
the transcript exists and is non-trivial before accepting the result.

The rule is provider-agnostic: it applies to `codex exec`, `agy`,
`cursor-agent`, `kiro` and any other external CLI a sub-agent shells out to.
