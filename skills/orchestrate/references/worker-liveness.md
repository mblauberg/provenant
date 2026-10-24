# Worker liveness

## Observe

Use `provenant lanes --all` for current state. A bounded wait uses
`provenant lanes --id RUN/TASK --wait SECONDS`; narrow output with `--limit`.
Inspect retained attempt bytes with
`provenant output RUN/TASK/ATTEMPT --part events --tail 100`.
Exit status and terminal records own completion; silence and elapsed time do
not prove death. Copy the attempt's resolved route exactly.

## Cancel

Use `provenant cancel RUN/TASK --operation-id ID --reason TEXT` only within
current authority. Cancellation is scoped to that target. Supervisor keeps
custody and writer exclusion through terminal publication and settlement.
Never signal a PID inferred from a stale record or an empty observation.

## Detached work

Run owns detached custody and recovery. Use `provenant reconcile RUN` for
recovery and `provenant resume RUN/TASK/ATTEMPT` with an authorised prompt for
continuation. Consult `provenant help` for operation IDs and prompt flags.
