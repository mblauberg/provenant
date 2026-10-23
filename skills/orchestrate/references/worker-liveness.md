# Worker liveness

Run the read-only helper as one command:

```bash
python3 "$(provenant root)/skills/orchestrate/scripts/worker_liveness.py"
```

By default it reports one row per dispatched `codex exec` worker in the current
Git repository. Use `--repo PATH` to select another repository. Wrapper
ancestors are collapsed into the real worker row, and `--cd` or `-C` determines
the reported worktree instead of the wrapper's launch directory. Each row shows
PID, elapsed wall time, accumulated CPU time, the newest matching
`~/.codex/sessions/**/rollout-*.jsonl` mtime and output size, worktree dirtiness,
and worker cwd. The `STALLED?` label is advisory. It never signals, kills,
restarts, watches or supervises a process.

Output growth is the positive liveness signal. Compare output size across
snapshots keyed to the owned PID: growth means progress, while no growth means
only that the chair must continue waiting or explicitly report/escalate. CPU
use, session-log mtime, an empty first snapshot and elapsed time do not prove
that a healthy API-bound worker is dead. A known unchanged output snapshot may
be labelled `STALLED?` as an advisory prompt, never as permission to terminate
or reuse the worktree. The one-shot helper reports the current output size; it
does not retain baselines or own the foreground wait. If more than one live PID
maps to the same worktree transcript, output attribution is ambiguous and the
helper reports no liveness size for that snapshot.

When an elapsed polling budget expires, re-arm the same bounded wait while the
owned PID remains live. Stop waiting only after the foreground wait observes
that PID's exit and captures its exit status. Do not replace this with a
watcher, scheduler, event bus or generic supervisor.

Detached harness-task state is not process state. Before terminal reporting,
inspection or reuse, confirm the run-owned worker PID is gone and its exit was
observed. Otherwise the detached task and a fresh worker can become two writers
in the same worktree. The existing foreground provider commands remain the
preferred waiting path; this guidance adds the fence and evidence, not a new
provider command.

The concrete terminal fence below is currently **Codex-only**: the helper's
`live_processes()` detector enumerates `codex exec` children. Use the provider's
own process boundary for agy, cursor, kiro or other adapters unless their
detector is extended. For a simple foreground call, the provider wait and its
returned status are the completion evidence; use this command only for a
detached protocol that captured `WORKER_PID` and `STATUS`:

```bash
python3 "$(provenant root)/skills/orchestrate/scripts/worker_liveness.py" \
  terminal-report --pid "$WORKER_PID" --classification complete --exit-status "$STATUS"
```

The command takes a fresh `ps` snapshot through the helper's existing
`live_processes()` detection. It emits a JSON terminal report and exits zero
only when the PID is no longer live and explicit exit evidence is supplied. A
live PID or missing exit evidence is reported to stderr and exits non-zero.
Use one of `blocked`, `question`, `unavailable`, `failed` or `complete` as
appropriate. This is a one-shot enforcement command, not a
watcher or supervisor.

The dispatch owner recognises a question only after observed provider exit,
adapter receipt validation and adapter exit `0`. The retained result must be
one exact JSON object: `{"schema_version":1,"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"..."}}`,
with a non-empty prompt of at most 4096 characters and no NUL.
Candidate parsing is bounded to a 64 KiB whole-result read, so larger results
remain ordinary. Never scan prose, question marks or Markdown fences. A malformed object using the
reserved record type fails closed as `terminal_envelope_invalid`; other prose
or JSON is ordinary output, and non-zero provider exit is never blocked.
Responding to a blocked attempt starts a new invocation and attempt, retaining
the original question and attempt unchanged.

For cross-terminal cancellation, write one empty `cancel.request` in the exact
attempt or batch directory through `skills/orchestrate/scripts/run_controls.py cancel`. The external command
never signals a PID or writes terminal evidence. The live dispatch owner polls
the marker during its bounded wait, stops its provider and tracked descendants
across process groups and sessions, and writes the normal cancelled attempt.
The owner snapshots ancestry about once per second and at termination, matching
PID and start time before signalling even after an observed child is reparented.
An inherited marker alone never authorises cleanup. Descendants outside the
provider group receive individual PID signals, never signals to their whole group. The same
cleanup runs on timeout, stall and normal exit. After observed provider exit,
children get up to 1.5 seconds to close naturally; survivors appear in the
attempt's `reaped` list and digest warning. A terminal event from a live CLI
skips that wait. Direct children exiting within 0.5 seconds are not reported;
direct children in another group still running then receive SIGTERM and another
0.5 seconds before SIGKILL. Other descendants get 0.5 seconds of SIGTERM grace,
while the root provider group gets up to 2 seconds to flush session state.
Census failure warns but cannot prevent the root group kill or terminal record.
A nested Fabric owner
and its observed subtree are spared only when `PROVENANT_RUN_DIR/dispatch-owner.json`
matches that process's PID, start time and `PROVENANT_RUN_TOKEN`. The owner must
lead its own process group and have an observed non-spared parent. A descendant with its
own session and a valid owner record is spared as an independent containment
boundary. If that owner ignores
SIGTERM after its host dies, the next dispatch's orphan reap handles it. The
provider's original group and the supervisor's group are never spared. Tracked
children are rechecked before signalling to cover fork followed by exec. Once
verified, the owner remains spared for the same PID and start time while alive,
even if its record or environment becomes unreadable. Code-mode hosts and their
observed subtrees are always spared; if a host shares the provider group, that
group is cleaned up by individual PID signals. A nonzero `spared` count
is recorded. A missing owner
yields bounded missing evidence; it never authorises a process-table search or
inferred cancellation.
A child that enters another session and reparents before its ancestry is observed
cannot be attributed safely by this polling owner. On Linux the owner is a child
subreaper only while an attempt runs, and restores its own setting afterwards.
An unavailable or permission-denied host probe excludes orphan reaping; it does
not establish host death. Unexpected signal deaths report `failed` without a terminal result, otherwise `interrupted`, with the
signal. Matching `termination-request.json` telemetry records the controller
source and requester, while the OS sender remains unknown. Spared processes
do not replace the Git and disk instruction check with a tamper failure.
An adopted orphan it cannot attribute to the attempt stays a zombie until the
owner exits: reaping it blindly could collect another thread's child.

Nested dispatch and batch are supported from worker lanes. The provider gets
`PROVENANT_PARENT_ATTEMPT`, containing the owning `run_id`, `task_id`, numeric
`attempt` and absolute `run_dir`. `PROVENANT_PARENT_RUN_TOKEN` binds local
registration to that run's owner token. Child run receipts and attempt rows retain
this object as `parent`; the parent attempt lists child run and task identities
in `children`. Registration uses a journal in the parent's private attempt
`tmp/children`, so a sandboxed worker need not write the parent's canonical
receipt. The owner copies the journal into its attempt before pruning temp
files and serialises registration with terminal publication. Local journal
entries must identify a real run under the runs root with a matching owner token
before the owner reads their receipts. Invalid entries are ignored with a diagnostic.

A direct nested CLI acquires run custody and publishes `dispatch-owner.json`
before re-executing with a fresh run token in its initial environment. It carries
the custody lock through exec, then detaches into its own process group only once
its published identity is verifiable. It remains a foreground command. This
supplies the same PID, start time and token evidence used to spare Fabric-launched owners. A busy caller
cannot replace the active owner's record. Child failures and missing terminal
evidence are named in the parent's `error`; they do not change the status
implied by the parent's own provider exit. Child entries are observations at
publication time, so a child still running when its parent ends normally stays
running in that retained snapshot. Parent cancellation cancels recorded local
children with a bounded grace period and descendant cleanup. The final cancellation
scan holds journal custody through terminal publication so late registrations
cannot escape it.
Peer placement carries the parent identity as transport metadata. The receiving
owner retains it without writing the source host's files; the source records
the qualified child identity and refreshes its status before publishing the
parent's terminal attempt. A failed peer status read is named as missing
completion evidence and preserves the parent's own provider status.

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

**Second choice, only when detachment is unavoidable: use the shared detached
helper.** Give each dispatch a unique run directory. The helper captures the
actual provider child PID in `worker.pid`, records its own wrapper PID in
`wrapper.pid`, starts the provider in a new session with stdin from `/dev/null`,
waits on the child directly, writes output to `run_dir/transcript.txt`, and
atomically writes the durable completion marker
to `run_dir/done`. The caller captures the wrapper PID separately:

```bash
run_dir=.agent-run/scratch/provenant-worker-<unique-slug>
"$(provenant root)/skills/orchestrate/scripts/run_worker_detached.py" \
  --run-dir "$run_dir" -- <worker command> &
WRAPPER_PID=$!
wait "$WRAPPER_PID"
STATUS=$?
WORKER_PID="$(cat "$run_dir/worker.pid")"
```

The helper claims `run_dir` exclusively, and its direct wait preserves the
provider exit status. If the original shell is gone, observe either the regular completion file or the
recorded wrapper exit. A wrapper exit without a marker is an evidence failure,
never a reason to accept or reuse the run:

```bash
helper="$(provenant root)/skills/orchestrate/scripts/run_worker_detached.py"
while :; do
  validation="$("$helper" --validate --run-dir "$run_dir" 2>/dev/null)"
  validation_status=$?
  if [ "$validation_status" -eq 0 ]; then
    break
  elif [ "$validation_status" -ne 1 ]; then
    echo "completion evidence missing or invalid; do not accept or reuse the run" >&2
    exit 1
  fi
  sleep 1
done
read -r WORKER_PID WRAPPER_PID STATUS <<< "$validation"
```

This fallback waits only on the claimed run directory's durable marker and the
recorded wrapper PID, and is used only after detachment; do not substitute a
watcher or notification for the direct PID wait. The marker is bound to the
claimed run directory and both recorded PIDs; a stale or concurrent marker must
never satisfy another dispatch. Confirm the worker PID is no longer live before
terminal reporting, inspection or reuse. The shared validator returns `1` while
the wrapper is still running and startup or completion evidence is incomplete,
`0` only for a structurally valid marker plus an observed worker exit, and any
other nonzero status is an evidence failure.

If a foreground wait times out, reissue that same wait while its PID or durable
completion marker remains available. Do not insert liveness probes or status
checks between reissues: each is a turn the worker could have finished in, and
the temptation to then stop and await a notification is exactly the failure
above.

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
