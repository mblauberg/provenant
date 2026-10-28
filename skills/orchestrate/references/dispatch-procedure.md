# Dispatch procedure

Load before submitting, resuming, cancelling or landing lanes. The entrypoint
keeps the coordination rules; this reference carries the mechanics.

## Submit and recover

For a terminal submission, use the core path:

```sh
provenant run --prompt-file brief.md --route senior
provenant status
provenant land 148
provenant quota
```

`run` submits, waits up to 240 seconds and prints one state line and the result
tail, with a footer for reading more. Set `--wait 30m` or `--wait 3600s` for a
longer budget and run that wait in the background. A timeout leaves the lane
running and prints the attempt ID and exact command to keep waiting. Follow
that command rather than submitting again. For a writer, add
`--branch feat/name --base main` to create its registered checkout;
`--worktree PATH` can select an existing one or create a new checkout.

`status` lists active and recent lanes newest first, with short ID, state,
model, age and reason. Use `--all` or `--json` for full records. `land N` checks
CI at the PR's current head before merging and pruning; pass `--head SHA` to
pin a reviewed head. `help --all` lists advanced commands and `help COMMAND`
shows all controls for one command.

CLI `run`, `dispatch`, `cancel` and `resume` generate and print an operation ID
when omitted. Supply `--operation-id ID` for planned retries and replay the
same ID, prompt and target after an uncertain submission. Attempts and runs
accept unique hex prefixes of at least eight characters and `RUN/ATTEMPT`;
ambiguous prefixes list candidates. Cancel and reconcile infer the project
from the run record.

The default Fabric MCP tools are `fabric_run`, `fabric_output`, `fabric_mail`
and `fabric_context`. Call `fabric_discover` with `{}` to inspect the schemas
for `fabric_graph`, `fabric_lease`, `fabric_task` and `fabric_events`, then call
those tools by name. Mail, identity and activity use `fabric_mail`,
`fabric_context` (`op: inspect`) and `fabric_events`. Advanced CLI recovery uses
`provenant lanes` for waits, `output ATTEMPT --part events|result|prompt|stderr
--tail N` for retained bytes, `reconcile --id RUN` and `fabric landing
push|reconcile`. A terminal without MCP cancels with `provenant cancel ATTEMPT`.

1. [Native first](routing-and-tiers.md#native-first): use native subagents for
   your own provider when the native tool supports the work. Claude callers use
   native Claude subagents; Claude models remain in the route pools for
   non-Claude chairs. Use Fabric for other providers, detached work and
   sandboxed worktree writers.
2. Call `fabric_run` with its `submit` operation; each task has one route
   choice. Supply `wait_seconds` to submit and wait in the same bounded call
   when the result is needed immediately. On timeout, retain the receipt and follow its continuation
   instructions. Use `provenant route list --adapter <id>` to inspect model ids
   and pool membership.
3. For a graph workflow, use `fabric_graph` with `op: run|status|cancel|resume`;
   pass the project-local graph document path to `run` and `resume`, and its
   `id` to `status` and `cancel`. Settlement advances dependants automatically.
   Routes accept optional `account`, including a different account for each
   fallback. Keep `resume` for manual recovery if an advancing process exits and
   no later run settles.
4. For writers, pass `access: "worktree_write"` and the registered `worktree` as
   an absolute path or relative to the selected repository's primary root, named
   per `setup-repo`'s branch naming doctrine; the task id defaults to that same
   branch-derived string. Before submission, [confirm the connection's
   project](fabric-identity.md), then acquire and verify an advisory
   `fabric_lease` `claim_acquire` (`scope` names the issue or path set, with an
   `operation_id` and `lease_seconds`); keep its generation/token proof, renew
   it while work continues and release it when finished. Do not submit against a
   conflicting live claim; submission does not enforce claims. One supervisor
   writer lease excludes a second writer; terminal Git sequencer state fails the
   lane. If custody is lost, keep the worktree reserved and follow [chair
   recovery](../../../docs/worktrees.md#writer-custody-recovery).
5. CLI `run` already waits and prints a result tail. For MCP or a batch
   submission, keep one bounded `provenant lanes --wait N` in the background;
   read the settled rows when it exits ([chair-loop.md](chair-loop.md#waiting)).
   A submitted sub-agent blocks on its worker in the foreground
   ([worker-liveness.md](worker-liveness.md)).
6. Use `provenant status` after compaction or restart; request full records
   with `--all` or `--json`. Inspect a terminal attempt with `fabric_run`'s
   `status` operation. Read extra retained bytes only when needed, using
   `provenant output ATTEMPT --part events` and its byte offset.
7. For a question, call `fabric_run` with its `resume` operation and the answer
   in `prompt`, targeting `target: { attempt: ATTEMPT }` and providing
   `expected_generation: 0` (it counts earlier continuations, not the attempt
   record's `generation`) and `operation_id`. On `Conflict`, read the target's
   `fabric_run` `status` with `detail: true`. If its `state` is not `terminal`, wait and retry. If
   you are retrying your own resume, repeat it with the same `operation_id`,
   prompt and target; the replay returns the existing continuation, including on
   a peer. Otherwise page through `fabric_run` `list` (local host only), then
   inspect candidates with `status` and `detail: true` to find the attempt
   whose `retry_of` is the target. Use a fresh `operation_id` only when
   the ID was reused with a different prompt. Continuations keep the prior
   attempt's applied provider, model and account; they do not draw the named
   route pool again.
8. Use `fabric_run`'s `cancel` operation with an explicit target to stop a run;
   use `fabric_output`'s `read` operation for a bounded slice. Output reads
   default to the result part and offset zero. Run operation IDs can be generated
   when omitted; retain the returned ID and supply it again for a planned retry.

## Dispatch details

- A rejected batch task returns its `error` and `fix`; other tasks in the same
  Rust Fabric submission may still run. Claude callers manage native Claude
  subagents outside the Fabric task list.
- Choose one exact model or named route for each Rust Fabric task. The bundled
  `utility`, `junior`, `senior`, `principal`, `writing` and `design` pools are
  weighted: operation ID and task index seed the draw, so a retry repeats its
  pick and zero-weight members are ordered fallbacks. Rust Fabric has no
  rotation, council-size, model-list, weight or synonym selector. For a [tiered
  council](tiered-council.md), submit separate tasks for distinct exact models
  and synthesise their results in the chair. The CLI's explicit model fallbacks
  are ordered recovery candidates, not council seats. `provenant route list`
  shows pool members and weights; readiness is decided at dispatch. An unknown
  exact model can reach its provider after activation, disclosure and confinement
  checks; named routes and pools require catalogue entries. An unsupported
  effort is refused.
- Dispatch defaults:
  - Fan out 2-4 independent lanes; go wider only for independent, checkable
    slices.
  - Before fan-out, run `provenant route list`. A named pool draws one member
    per task by weight and does not guarantee family diversity. For family
    diversity, select distinct exact model IDs from the listing and submit one
    task per model. Set effort only when the catalogue records that exact
    control.
  - Set the exact model for each native subagent when its tool allows it. Claude
    callers use native Claude subagents; non-Claude chairs can dispatch Claude
    pool members through Fabric. Brief native subagents with paths; end each
    when its deliverable lands.
  - Brief in five fields: goal, scope/paths, constraints, output shape and
    length, verification.
  - agy lanes are OS-confined to their `cwd` and `add_dirs`; put the brief
    inline in the task `prompt` (`prompt_file` is CLI-only), rather than paths
    for the lane to open.
  - Keep tool output bounded: output over a bound goes to a file, and the
    command returns a pointer and a short tail. Avoid unbounded `gh ... --limit
    1000` and `rg --files` over run directories. A polling loop prints one line.
  - CLI `run` combines submission, a bounded wait and the result tail. For
    an existing or multi-lane selection, use one background `provenant lanes
    --wait N`; for landing, use `provenant land N`. Never sleep or poll.
  - No subagent or lane blocks in a foreground wait longer than about four
    minutes. Long waits run in the background (the harness notifies on
    completion) or belong to the chair. Cache lifetimes vary by provider
    (Anthropic's default is five minutes; others differ), so keep waits short
    and let receipts record cache reads.
  - Shape briefs for the cache: stable shared preamble and context pack first,
    task-specific content last. Resume the lane's session for follow-ups rather
    than dispatching fresh, and let sibling lanes share one context pack. Cached
    reads cost roughly 5 to 50 times less than fresh input, depending on
    provider.
  - Size lanes by context, not minutes: one logical unit and one commit per
    lane. The lane keeps a short progress notes file; near its context budget it
    stops, and the chair restarts it fresh from the brief, contract and notes
    rather than letting it work from a rotted context.
  - Research: when a decision rests on external facts (docs, papers, prices,
    prior art), dispatch research lanes with network access proactively, fanned
    out across juniors with a senior synthesis.
  - Chair judgement, not config: no code enforces these, so apply them yourself.
    Lean toward the lower figure when saving usage and the higher one for
    rigour.
    - Effort: keep the route's recorded effort; raise it one step for `crucial`
      work, or lower it one step for mechanical lanes, only to an effort the
      catalogue records for that model.
    - Council: about 3 juniors per part in a [tiered council](tiered-council.md)
      (2 to save usage, 4 for rigour).
    - Repair: stop at the risk tier's [repair
      budget](../../deliver/references/contract.md#repair-budget), or sooner to
      hand back to the user.
    - Summaries: cap a worker's returned summary at about 2k tokens (1k when
      lean); the file stays the record.
    - Pools: take the pool's weighted draw. To spread quota or families, submit
      distinct exact models instead.
    - Prose polish: reader-facing prose may take one final voice pass
      ([engineering-writing](../../engineering-writing/SKILL.md) step 6); skip
      it to save usage.
    - Context: providers own compaction; keep a lane to one logical unit instead
      of setting a ceiling.
    - Quota pace: read a window within about 4 points of even use as on pace;
      further ahead is pressure to reroute.
- Every enabled lane, including junior lanes, receives Provenant's generated
  Fabric MCP configuration and supported hooks. Nested MCP submission admits
  child records within `lanes.max_depth` and attempts the shared launch path.
  Live nested provider launch and utility-model execution remain unverified and
  subject to provider activation gates; do not assume submission means
  execution. Dispatch sets `PROVENANT_LANE_DEPTH` and records the parent
  attempt; `lanes.max_depth` defaults to 2 and refuses deeper nesting. User MCP
  servers and personal hooks are excluded.
- A dispatched lane has no inbox. Peers message sessions, not lanes; a lane gets
  new instructions only through `resume` or a new dispatch.
- For reviews that need history, pass `--base REF` to `provenant run`, or
  `base` on each manifest or Fabric submission task. The host writes bounded log
  and diff files into the attempt's temporary scratch directory and names their
  paths in the prompt. Reviewers read `git-context.json` first for the resolved
  base, captured HEAD and truncation flags, then `git-log.txt` and
  `git-diff.patch`. Report incomplete coverage when either file is truncated or
  `timed_out` is set. These files describe committed changes; inspect the
  checkout separately for uncommitted edits. They are removed at settlement, so
  put review evidence in the final answer. Peer writers (`--host` with
  `--branch`) get no context files.
- A chair running lanes across many wakes follows
  [chair-loop.md](chair-loop.md), including its reviewer git-verb denylist and
  decision council.
- Before landing, acquire a work claim if none is live for this session, verify
  it, and call `fabric_lease` `landing_acquire` with the integration reference,
  expected remote OID, candidate OID, `lease_seconds` and operation ID. See the
  [landing lease runbook](../../../docs/runbooks/landing-lease.md). Verify
  pinned candidate and integration OIDs through Rust confinement before merging.
  Do not infer a protected-tip release from an advisory claim. Renew or release
  the lease with `fabric_lease`, its generation/token proof and an operation ID.
  A project whose landing tool owns the lease leaves it to that tool.
- Keep full worker output in run files and return only the digest and path;
  brief native subagents to the [~150-word result
  digest](orchestration-contract.md#native-result-digest). For liveness, use
  `fabric_run`'s `status` operation; see
  [worker-liveness.md](worker-liveness.md) for degraded runs. Size alone proves
  nothing.
- On a terminal review, record the exact provenance line from
  `provenant output RUN/TASK/ATTEMPT --part receipt`. Reviews use `Reviewer:`;
  writer provenance uses `Writer:`. Include the following `Reviewer provenance:`
  line for the canonical actor and observed attribution. Preserve `unknown` route,
  effort, family and resolution evidence rather than guessing them. Native subagents record `claude/<model>@<effort> (anthropic;
  resolved)` from the Agent tool's model parameter, never from self-report.

## Fabric dispatch brief template

Bind the [worker contract](orchestration-contract.md#worker-contract), then fill
these task-specific fields inline in the Fabric task `prompt`:

```text
Goal: <bounded task>
Scope/paths: <target, base/head and allowed paths>
Constraints: <authority, isolation, governing skill and stop condition>
Output: <shape, length and durable evidence location>
Verification: <checks; for review, the rubric path or full rubric>
Review provenance: the chair records `provenant output RUN/TASK/ATTEMPT --part receipt` after settlement.
Cargo (writer lanes): batch targeted tests into one invocation
(`cargo test -p a -p b FILTER`) and never run two cargo commands on the same
target directory at once. Run `cargo check` and `cargo clippy` with both
`CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` set to
`$PROVENANT_LANE_CARGO_LINT_TARGET`, so they can run beside a test build. Keep incremental builds on: the sandbox's hard-link
warning only means rustc copies instead.
Agent-filed issues: when filing is authorised, apply source:agent and end the
issue body with Filed-by: <PROVENANT_AGENT_LABEL> <run> using the actual label
and originating run ID. This applies to any issue an agent files.
```

Review briefs follow the [review outcome](../../code-review/references/finding-contract.md#review-outcome) rule.
