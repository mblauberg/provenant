# Recovery and cadence

Use the run and Fabric as the source of live execution state. Session text may
be compacted or lost; work is recoverable through the run's task records and
outputs, with a short keyed Markdown checkpoint for human-readable context.

## Interruption and resume

After a crash, context loss, or uncertain dispatch:

1. Run `provenant reconcile` to recover queued or orphaned work.
2. Inspect `provenant lanes` and the related Fabric tasks.
3. Use `fabric_run` `resume` (`expected_generation: 0`) to continue the attempt
   in a new run that has it as parent; on a conflict, target the child attempt.
   Read task outputs and verify the exact target before repeating any work.
4. Preserve useful partial outputs. Retry only the failed or missing work, and
   stay within the retry bound owned by the relevant lifecycle.
5. Record the blocker, evidence, and next trigger in the host-session
   checkpoint when it will help a later resume.

Do not relaunch work merely because the current conversation lacks a result.
Do not treat a worker's success message as proof; inspect its artifact and run
the required objective checks. An unavailable provider, failed task, and
unverified result remain distinct states.

## Cadence and idle work

Respect run-owned admission, wait, and pause behavior. Do not poll unchanged
state, busy-dispatch into a known limit, or invent work to keep an iteration
active. If no eligible or active work remains, describe the waiting condition
and what event can resume it, then end the current invocation. This does not
complete or close the mission. The mission remains open until the user sets
`STATUS: STOP`.

When new instructions, a gate answer, external completion, or material change
arrives, reconcile the run before assigning work. Keep the user-visible status
accurate and preserve the evidence for any residual.

## User STOP and handoff

Check the user-owned goal each iteration. Only the user may set `STATUS: STOP`.
After that instruction, reconcile active work, verify completed artifacts and
checks, and provide a handoff listing evidence, remaining work, blockers, and
the next owner. A dry frontier, a successful task, or an idle run is never a
substitute for STOP.
