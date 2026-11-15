# Herdr panes

Herdr observes interactive work and sends fire-and-forget steering. Fabric owns
answer-bearing assignments and retained results. Pane telemetry and scrollback
are advisory; neither proves completion or review acceptance.

## Preflight

Operate from a Herdr-managed pane and confirm `HERDR_ENV=1`; an installed CLI
alone does not grant control of the user's workspace. Check installed help before
using session controls. Incomplete mutating commands may execute defaults.
Target the current pane explicitly or use its returned ID, not a display
position: an omitted target may select another client's focus. Preserve the
caller's focus.

## Start and observe

Split a pane, start the agent at its interactive shell prompt and inspect its
readiness. Record returned pane and terminal IDs. Readiness does not certify a
result. For shell commands use a unique completion marker and a bounded wait;
do not poll. Collect the correlated Fabric reply or durable run result and verify
its evidence.

## Steering and results

Use Herdr only to steer an already tracked assignment. Send answer-bearing work
through Fabric with correlation and named output artefacts. Successful transport
does not prove uptake. Literal `\r` is text, not Enter; a session UUID is not a
pane ID; scrollback can discard long results, so persist them before replying.

When Fabric cannot carry the request and reply, record
`FABRIC-ROUNDTRIP-UNAVAILABLE`, a named artefact and the bounded collection step.
Herdr remains an observation surface without an automatic callback.

## Finish

Verify the result, preserve a bounded digest, then close or reuse only resources
created by the run within its authority. Keep other panes and the server running.
Discard IDs after closure and use returned IDs after a move.
