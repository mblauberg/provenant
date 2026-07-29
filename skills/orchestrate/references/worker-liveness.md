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

## Waiting from inside a sub-agent

A sub-agent that ends its turn is finished. Its result returns to the caller at
that moment, and no later notification reopens it. A background wait therefore
only works for a chair sitting in the main loop; from a sub-agent it silently
discards the run.

This is the single most common dispatcher failure. It has now occurred nine or
more times across `codex`, `agy` and `cursor-agent` lanes, always the same way:
the worker is launched detached, a background watcher is armed, the turn ends
with "I will wait for the notification", and the caller receives a progress
report instead of a result while the worker keeps running unattended.

So a dispatching sub-agent blocks in the **foreground** and never arms a
background watcher for its own worker. Prefer the cheapest form that fits.

**First choice: run the worker in the foreground.** No detaching, no PID, no
polling. The shell blocks on process exit and the harness holds the turn open:

```bash
codex exec -s <sandbox> -C <ABSOLUTE_DIR> -m <model> - < brief.txt > out.txt 2>&1
```

Give it the largest timeout the tool accepts. This is the whole procedure when
the run fits inside one timeout window.

**Second choice, for runs that can exceed that window: detach, then block on a
FIFO.** Still event-driven and still zero-poll, and it carries the exit status:

```bash
mkfifo done.fifo
nohup bash -c '<worker command> > out.txt 2>&1; echo EXIT=$? > done.fifo' >/dev/null 2>&1 &
```

then, as a separate foreground call, `cat done.fifo`. It blocks on the write and
returns the worker's exit code. If the tool timeout fires first, reissue `cat`
unchanged; the FIFO is still there and still unwritten.

**Last resort**, where a FIFO is awkward, a foreground condition loop:

```bash
while kill -0 <PID> 2>/dev/null; do sleep 20; done; echo WORKER-EXITED
```

Harnesses that block a bare foreground `sleep` still permit this form, because
the guard targets `sleep N; <command>` poll chains rather than a loop blocking
on a condition. It polls, so prefer either option above it.

Whichever form is used, reissue on timeout and do not substitute short sleeps,
liveness probes or status checks between reissues: each is a turn the worker
could have finished in, and the temptation to then stop and await a notification
is exactly the failure above.

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
The chair is the only party that may legitimately wait in the background,
because the chair is still there to be woken.
