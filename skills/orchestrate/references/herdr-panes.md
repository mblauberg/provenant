# Herdr pane operating contract

Herdr is orchestrate-owned progressive disclosure for observing, waking and
degraded manual control of substantial interactive, long-running and
cross-family work. It does not replace native subagents, carry authoritative
answers or decide model routing.

## Source grounding

The mechanics below were independently checked against the installed Herdr CLI
on 15 July 2026. The installed CLI and its runtime command surfaces are the
syntax authority. Provider versions are not compatibility gates. The upstream
[`SKILL.md` at commit `c76e968`](https://github.com/ogulcancelik/herdr/blob/c76e96878b866bf01639c8a1d8beb9c93a8ab95f/SKILL.md)
was consulted on 2026-07-15, not copied or vendored. That upstream component is
licensed AGPL-3.0-or-later or commercially; this repository has no runtime or
mutable-branch dependency on it.

## Use a pane when

- the worker may run longer than about two minutes;
- the user benefits from watching or steering it;
- it may block for input;
- it is the other primary family or a distinct-family/adversarial review leg
  worth observing;
- the task must survive the lead's context churn.

Keep short mechanical/native fan-out inline. For substantial eligible work done
without Herdr, record `HERDR-NOT-USED: <reason>`.

## Preflight

```sh
test "${HERDR_ENV:-}" = 1
herdr status server
herdr integration status
herdr agent list
```

Operate the live session only from a Herdr-managed pane. If `HERDR_ENV` is not
`1`, report that boundary instead of inspecting or controlling the user's
focused session from outside Herdr.

Treat the installed CLI as the syntax authority. Use `herdr --help` and the
relevant non-mutating command-group help. Calling `herdr` without arguments
opens the UI. Supply every required argument when checking nested commands;
an incomplete mutating command can execute its defaults.

Workspace, tab, pane and terminal IDs are opaque. Parse them from JSON after
every create, split or move; never derive them from display order or examples.
Use `--current` for the caller pane or an explicit returned ID. An omitted
target may resolve to another client's focused pane.

Require a compatible running server and a current integration for the selected
agent. An installed CLI without an integration can still run, but its state is
`unknown`; do not treat that as reliable completion telemetry. Pi remains
dormant until its provider/model route is approved and `herdr integration
install pi` has been evaluated.

Herdr never grants worktree authority. Create or remove a worktree only under a
user-approved authority envelope, and only at the owning repository's
`.worktrees/<task-agent>` path. Herdr control cannot broaden model-routing,
disclosure, receipt, cleanup or resource-ownership rules.

## Place, start and name

Inspect the caller's layout before choosing a split:

```sh
herdr pane layout --current
```

Explicit user direction wins. Otherwise choose `right` or `down` from the
returned pane geometry so neither child is needlessly cramped. Preserve the
caller's focus with `--no-focus` unless the user asks to move it.

Discover the current CLI/model options first. Start the selected agent's normal
interactive executable without a prompt, record the returned pane and terminal
IDs, wait once for the agent-level `idle` readiness state, inspect the pane, and
only then steer it:

```sh
split_direction=right  # or down, chosen from the returned layout
herdr agent start review-other-primary --cwd "$PWD" \
  --split "$split_direction" --no-focus -- claude
herdr agent wait review-other-primary --status idle --timeout 30000
herdr agent read review-other-primary --source recent-unwrapped --lines 80
```

Use role names such as `review-claude`, `review-codex`, `scout-gemini` or
`implement-api`. Paired-primary runs use `pair-claude` and `pair-codex`; both
panes remain owned by the session chair when stage ownership rotates. Record
the pane/terminal ID and actual adapter, provider family
and resolved model in the run receipt.

## Send steering

Use the bundled helper only for fire-and-forget steering with no expected
answer. It is a thin client for the authenticated public Fabric operation;
Fabric validates the exact task or message revision and target before its
daemon-owned Herdr integration performs any pane I/O. Its terminal result is
`dispatched-unconfirmed` with `referenceValidation: verified`, not proof that
the target process consumed the prompt:

```sh
printf '%s' '<bounded prompt>' | \
  "${AGENTS_HOME:-$HOME/.agents}/skills/orchestrate/scripts/herdr_prompt.sh" \
  <name> --fire-and-forget --action-id <stable-action-id> \
  --pane-ref <pane-id> --task-ref <task-id> --expected-revision <revision>
```

The helper never waits and cannot return an answer. Do not use it for an
assignment, review, research request or any work the lead must consume. Prefer
it over separate raw text/key sends only for steering an already tracked task.
The helper has no direct Herdr fallback. Unknown, stale, target-mismatched or
answer-bearing references fail before pane I/O; unavailable Fabric or Herdr
fails closed with a typed result. Exact retries reuse the stable action without
duplicating the pane effect.

## Send answer-bearing work

Before waking a peer, create a structured Fabric task and request message with
the owning task/revision, conversation and message IDs, expected output or
artifact, `reply_to`, acknowledgement requirement, dedupe key and deadline.
Herdr then wakes or focuses the peer; it is not the transport of record.

The peer consumes and acknowledges the request through Fabric at a safe turn
boundary. Reply, terminal task result and pending request-result delivery commit
in one transaction or transactional outbox before completion-ready. The
requesting chair/lead integration subscribes to the correlated terminal result
and receives it at its next safe turn boundary. An idle requester is woken; an
active requester is not interrupted mid-tool or mid-turn. The Console may
display the committed reply before requester consumption, but does not
acknowledge it on the requester's behalf.

Response deadlines persist as barrier-blocking obligations. On overdue work,
the chair may retry the same stable action, reassign or abandon with reason; it
never blindly redispatches. Late replies remain evidence but do not silently
complete superseded work. Claim generation and stable callback IDs make
provider acceptance and requester consumption idempotent across restart.

If the active integration cannot provide Fabric request/reply, record
`FABRIC-ROUNDTRIP-UNAVAILABLE`, use a named artifact plus an explicit bounded
collection step, and report the degraded manual path. Never describe pane
status or scrollback as an automatic callback.

Observed raw-send failure modes (2026-07-10, one session lost ~20 min to these —
use the helper instead):

- `herdr agent send <pane> '\r'` sends the two literal characters `\r`, never
  a submit; the draft sits in the composer looking sent.
- Targeting by session UUID fails with `agent_not_found` — target the pane id
  (`w3:p2`), not the session id.
- The pane buffer scrolls: output longer than the buffer is unrecoverable via
  `read` — ask the worker to re-emit only the missing span, or have it write
  findings to a file from the start.
- `pane run` can still leave a long draft unsubmitted. The helper follows it
  with a settled Enter; still read the pane after dispatch because successful
  transport does not prove agent uptake.

Prompts state source scope, artifact directory, allowed artifact classes, source
write authority, output contract, evidence requirements and whether subagents
are permitted. For long-running work, prefer artifact-only writes plus a
compressed return over full output in the lead context. Do not paste secrets or
uncleared project data into an external provider pane.

For paired-primary work, persist the assignment through Fabric and use Herdr
only for the bounded wake-up notification. The durable envelope records
task/stage, chair, owner, peer, base revision, write scopes, prohibited actions,
expected output, objective checks and user gates. The peer replies through the
correlated Fabric exchange and writes any named artifact; pane scrollback is
never the authoritative negotiation.

## Observe ordinary commands

For an ordinary command, make it emit a unique terminal marker. Read a bounded
tail first. Only when the marker is absent and the lead has no other useful work
should it wait once, with a deadline, then read the bounded tail again:

```sh
herdr pane read "$pane_id" --source recent-unwrapped --lines 80
herdr wait output "$pane_id" --match '<terminal marker>' \
  --source recent-unwrapped --lines 80 --timeout 120000
herdr pane read "$pane_id" --source recent-unwrapped --lines 80
```

This output wait is the ordinary-command completion path. It is not agent
completion evidence and never upgrades scrollback into an authoritative result.

## Observe steering without poll loops

- Continue useful local work after dispatch.
- Check once near expected completion with `herdr agent get <name>` or
  `herdr agent list`.
- If the main thread truly has nothing else to do, use one bounded
  `herdr agent wait <name> --status idle --timeout <ms>`.
- Read only the bounded tail: `herdr agent read <name> --source
  recent-unwrapped --lines <n>`.

Do not repeatedly wait in 20–30 second loops. State telemetry is advisory and
can be stale. If status conflicts with a live session, inspect
`herdr pane process-info`, `herdr agent explain`, and a bounded pane tail before
declaring failure.

Keep the two wait surfaces distinct. `idle` is the agent-level integration
state used by `herdr agent wait <name> --status idle`; that command does not
accept `done`. The `done` pane-level unseen-completion attention state is used
by `herdr wait agent-status "$pane_id" --status done --timeout <ms>`. Neither is
proof of a correct result: inspect the pane record and bounded output, then use
the Fabric reply or named artifact. A `blocked` agent needs input; `unknown`
means integration or detection is not yet reliable.

For answer-bearing Fabric work, await or subscribe to the correlated terminal
result instead of polling pane state. Delivery to the lead occurs at a safe turn
boundary and survives lead compaction or restart as unread Fabric state.

## Finish

Capture the worker's concise result and artifact paths, adjudicate them in the
lead context, preserve a bounded status/failure receipt, then close or reuse the
pane deliberately. Only close panes, tabs, workspaces or sessions created by
this run; touching another owner's resource requires an explicit user request.
Never stop the Herdr server from an active session as incidental cleanup. Do not
retain a full pane transcript when the named artifact and receipt are sufficient.
Clean pane-local temporary payload only when the run owns it; lifecycle and
retention follow `session`'s context-hygiene contract. A pane being idle does
not prove its work is correct; objective checks and review still gate the result.

After closing a pane, discard its pane and terminal IDs. After a cross-workspace
move, replace cached workspace, tab and pane IDs with those returned by Herdr;
never reuse the old tuple. For caller-context receipts, the stable environment
keys are `HERDR_ENV`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` and,
when endpoint lineage matters, `HERDR_SOCKET_PATH`. They describe context, not
authority, and cached values are not a lookup mechanism after a move.
