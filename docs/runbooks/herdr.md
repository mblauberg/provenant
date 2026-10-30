# Herdr panes

Owner: orchestrate
Anchor: skills/orchestrate/SKILL.md
Source: local CLI inspection, 30 October 2026 (Herdr 0.9.5). Recheck installed help before use.

Herdr observes interactive work and sends fire-and-forget steering. Fabric owns
answer-bearing assignments and retained results. Pane telemetry and scrollback
are advisory; neither proves completion or review acceptance.

## Preflight

Operate the live session from a Herdr-managed pane. Confirm `HERDR_ENV=1`
before using session controls; an installed CLI alone does not grant control
of the user's focused workspace.

```sh
herdr --help
herdr status server
herdr integration status
herdr agent list
herdr pane layout --current
```

Use the installed CLI's non-mutating help to confirm syntax. Calling
`herdr` without arguments opens the UI. Incomplete mutating commands may
execute defaults, so supply every required argument.

Read workspace, tab, pane and terminal IDs from returned JSON. They are opaque,
not display positions. Use `--current` or an explicit returned target; an
omitted target may select another client's focus. Keep the caller's focus
with `--no-focus` unless directed otherwise.

## Start and observe

Choose `right` or `down` using the caller's layout. Split the pane, set
`pane_id` to the returned pane ID, then start the selected agent in that
pane at its interactive shell prompt and inspect its readiness:

```sh
herdr pane split --current --direction right --cwd "$PWD" --no-focus
herdr agent start review-peer --kind claude --pane "$pane_id"
herdr agent wait review-peer --until idle --timeout 30000
herdr agent read review-peer --source recent-unwrapped --lines 80
```

Record the returned pane and terminal IDs. Agent-level `idle` describes
integration readiness; `unknown` means the integration cannot establish it.
Use `herdr agent wait review-peer --until done --timeout 30000` to wait for
the `done` state. Neither state certifies a result.

For ordinary shell commands, use a unique completion marker and a bounded
tail. If there is no independent work, wait once with a deadline:

```sh
herdr pane read "$pane_id" --source recent-unwrapped --lines 80
herdr pane wait-output "$pane_id" --match '<completion marker>' --timeout 30000
```

Do not build a polling loop around pane status. For agents, collect the
correlated Fabric reply or durable run result and verify its evidence.

## Steering and results

`herdr pane send-text "$pane_id" 'bounded steering'` sends literal text without
submitting it. To submit a prompt, use `herdr agent prompt TARGET 'bounded
steering' --wait --timeout 30000`. Neither proves uptake or returns an answer.
Use these only to steer an already tracked assignment. Send answer-bearing reviews, research and peer
tasks through Fabric; preserve correlation and named output artefacts.
Use `provenant status` and `provenant output TARGET` for run evidence.

Retain these observed failure boundaries:

- Literal `\r` is text, not Enter.
- A session UUID is not a pane ID.
- Scrollback can discard long results; persist the result before replying.
- Successful transport does not prove the agent consumed a prompt.

When Fabric cannot carry the request and reply, record
`FABRIC-ROUNDTRIP-UNAVAILABLE`, a named artefact and the bounded collection
step. Herdr remains an observation surface, without an automatic callback.
Use `orchestrate` for routing, authority, writer worktrees and fan-in.

## Finish

Verify the named result, preserve a bounded digest, then close or reuse only
resources created by the run within its authority. Keep other panes and the
server running. Discard IDs after closure, and replace cached IDs with those
returned after a move. Cleanup follows the repository workflow and authority.

The upstream [Herdr skill at c76e968](https://github.com/ogulcancelik/herdr/blob/c76e96878b866bf01639c8a1d8beb9c93a8ab95f/SKILL.md)
was consulted on 15 July 2026 for methods, without copying or vendoring it.
Its Apache-2.0 licence grants no runtime dependency or authority.
