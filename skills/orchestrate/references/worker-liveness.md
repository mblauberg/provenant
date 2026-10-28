# Worker liveness

## Observe

Use `provenant status` for active and recent lanes, newest first; `--all` or
`--json` returns full records. CLI `run` already waits and prints a result tail.
On timeout, follow its printed command to keep waiting on the same attempt.
For an existing multi-lane selection, a bounded advanced wait uses
`provenant lanes --id RUN --wait SECONDS`; narrow output with `--limit`.
Inspect retained attempt bytes only when needed with
`provenant output RUN/ATTEMPT --part events --tail 100`.
Exit status and terminal records own completion; silence and elapsed time do
not prove death. Copy the attempt's resolved route exactly.

## Cancel

Use `fabric_run`'s `cancel` operation (`target: { attempt }`, `operation_id`)
only within current authority. A terminal without MCP uses `provenant cancel
ATTEMPT [--operation-id ID] [--reason TEXT]`. Unique hex prefixes of at least
eight characters and `RUN/ATTEMPT` are accepted; an ambiguous prefix lists
candidates. CLI cancellation generates and prints an operation ID when omitted
and infers the project from the run. Keep an explicit ID for planned retries.
The advanced `RUN/TASK/ATTEMPT` form selects one attempt; `RUN/TASK` selects
every attempt of that task. A remote placement ignores `--reason`. Supervisor keeps custody and
writer exclusion through terminal publication and settlement.
Never signal a PID inferred from a stale record or an empty observation.

## Detached work

Run owns detached custody and recovery. Use `provenant reconcile --id RUN` for
recovery; the run record supplies project scope. Resume an attempt with `fabric_run`'s `resume` operation (`target: {
attempt }`, `expected_generation: 0`, `operation_id`, `prompt`).
`expected_generation` counts earlier continuations, so pass 0; it is not the
attempt record's `generation`. On `Conflict`, read the target's `fabric_run`
`status`. If its `state` is not `terminal`, wait and retry. If you are retrying
your own resume, repeat it with the same `operation_id`, prompt and target; the
replay returns the existing continuation, including on a peer. Otherwise use
`fabric_run` `list` (local host only) and target the attempt whose `retry_of` is
the target. Use a fresh `operation_id` only when the ID was reused with a
different prompt. CLI `resume ATTEMPT --prompt-file followup.md` generates and
prints an operation ID unless supplied. Resume pins the prior applied provider,
model and account, even when its original route was a pool. Use an authorised
prompt for continuation.
