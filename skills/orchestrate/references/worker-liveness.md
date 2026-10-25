# Worker liveness

## Observe

Use `provenant lanes --all` for current state. A bounded wait uses
`provenant lanes --id RUN/TASK --wait SECONDS`; narrow output with `--limit`.
Inspect retained attempt bytes with
`provenant output RUN/TASK/ATTEMPT --part events --tail 100`.
Exit status and terminal records own completion; silence and elapsed time do
not prove death. Copy the attempt's resolved route exactly.

## Cancel

Use `fabric_run`'s `cancel` operation (`target: { attempt }`, `operation_id`)
only within current authority; a terminal without MCP uses `provenant cancel
RUN/TASK --operation-id ID --reason TEXT`, which cancels every attempt of the
task, whereas `fabric_run` cancels one attempt. Supervisor keeps custody and
writer exclusion through terminal publication and settlement.
Never signal a PID inferred from a stale record or an empty observation.

## Detached work

Run owns detached custody and recovery. Use `provenant reconcile --id RUN` for
recovery. Resume an attempt with `fabric_run`'s `resume` operation
(`target: { attempt }`, `expected_generation: 0`, `operation_id`, `prompt`).
`expected_generation` counts earlier continuations, so pass 0; it is not the
attempt record's `generation`. Use an authorised prompt for continuation.
