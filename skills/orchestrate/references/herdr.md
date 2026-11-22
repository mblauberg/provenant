# Herdr panes

Herdr observes interactive work and sends fire-and-forget steering. Pane
telemetry and scrollback are advisory: neither proves completion or review
acceptance. Send answer-bearing work through `provenant fabric mail` (a
`request` answered with `--reply-to`) or a lane result, name the output
artefacts, and collect that reply; successful transport does not prove uptake.

## Preflight

Operate from a Herdr-managed pane (`HERDR_ENV=1` and a pane ID); an installed
CLI alone does not grant control of the user's workspace. Check installed help
before using session controls, because an incomplete mutating command may run
its defaults. Target the pane by the ID Herdr returned, never a display
position: an omitted target may select another client's focus. Preserve the
caller's focus.

## Run and observe

Split a pane, start the agent at its interactive shell prompt and record the
returned pane and terminal IDs. Readiness does not certify a result. For shell
commands use a unique completion marker and a bounded wait. Literal `\r` is
text, not Enter; a session UUID is not a pane ID; scrollback can discard long
results, so persist them before replying.

## Finish

Verify the result, keep a bounded digest, then close or reuse only panes the
run created. Leave other panes and the server running, and use returned IDs
after a move.
