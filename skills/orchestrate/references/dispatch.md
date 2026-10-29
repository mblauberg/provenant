# Dispatch and fan-in

Use presets before constructing a graph. The [preset runbook](../../../docs/runbooks/presets.md)
owns expansion, node syntax, subject binding and limits; the
[CLI reference](../../../docs/reference/cli.md) owns flags. This reference
covers the chair's choices that those mechanisms cannot make.

## Brief

Give each lane a bounded assignment, for example:

```text
Outcome: replace the approved parser implementation, preserving its wire format.
Spec and authority: task #1234; write src/parser and its existing tests only.
Source: full base SHA and the registered writer worktree.
Read: src/parser, callers, format specification; no credentials or unrelated paths.
Must preserve: published input/output contract and acceptance criteria.
Verify: existing parser contract tests and compiler; record exact results.
Return: commit SHA, changed paths, criterion evidence, risks and remaining work.
Stop: an unexplained wire difference, missing authority or the lane budget.
Delivery: local commit only; chair owns integration and external actions.
```

Name dependency seams before parallelising. Two writers changing the same
schema and its callers are coupled until that seam has landed. A read-only
hypothesis fan-out can run while a single writer owns the eventual fix.
Include expected output paths and collection responsibility, so a result
survives truncation without depending on terminal scrollback.

Use native agents for suitable same-provider work. Send answer-bearing work
for other providers through Fabric. A durable run result is the normal return;
interactive peer requests need a correlated Fabric reply. If that exchange is
unavailable, record `FABRIC-ROUNDTRIP-UNAVAILABLE`, the named result artefact,
and the bounded collection step. Herdr only observes and steers.

## Submit and continue

Inspect the effective preset, including project overrides, before previewing
the exact invocation:

```sh
provenant preset list
provenant preset show generator-evaluator
provenant preset explain generator-evaluator
provenant run --preset generator-evaluator --prompt-file brief.md \
  --worktree /absolute/path/to/writer \
  --set 'parameters.scope=["src/parser"]' --explain
```

Submit the same invocation without `--explain`. The preview creates no graph
state. Identical resolved graph submissions observe the same graph; change
the input only when a new run is intended. A wait timeout leaves work running.
Keep the returned graph ID and document, then follow its continuation command.

```sh
provenant status
provenant output TARGET
provenant fabric graph status GRAPH_ID
provenant fabric graph resume --document PATH
provenant fabric graph cancel GRAPH_ID
```

Status observes; resume advances the retained graph, including pending CI
gates. An exhausted loop needs an explicit chair decision, with authorised
extensions supplied by `provenant fabric graph resume --document PATH --extend KEY=VALUE`.
The runbook defines accepted extension keys. Keep host ceilings and the
approved budget intact. A changed source pin requires a new graph on the new
head, rather than pretending earlier review applies.

Distinguish pending CI, a failed attempt, an exhausted loop and a human
decision. Each has a different resume condition. Reconcile the retained
state before replacing work after an interruption. A failed child blocks
downstream work; synthesis cannot certify around a failed gate.

## Fan-in

Collect a digest and retained evidence paths. Verify the actual commit,
changed paths, acceptance checks, review subject and resolved route against
the live tree and run output. Tests prove the exercised properties; inspect
uncovered criteria separately. Discard duplicated or unsupported findings.

Integrate disjoint writer heads in dependency order. Re-run affected checks
and review changed integration surfaces after the tree changes. For
`big-batch`, supply `lanes`, `batch_worktree`, `base`, `spec` and
`integration_branch` through a parameter file. Each lane entry has `name`,
`worktree`, `scope` and `spec`. The council reads the pinned combined head;
serial refactoring and repair use the batch worktree.

When landing is authorised, `provenant land N --head SHA --wait` gates the
exact PR head. Keep production promotion and other reserved actions at their
own authority gate. A graph's passing verdict does not widen the envelope.

## Ready-issue queue

Read the canonical tracker queue; dispatch only approved issues with stable
acceptance, satisfied dependencies and disjoint write scopes. Cap active lanes
by available resources and useful independence, rather than a fixed refill
target. Revalidate readiness at admission. `tracker` owns issue placement and
dedupe; `implement` owns software delivery.

Keep assignments and live status in Fabric and runs. Record milestones through
`session`; keep no second board or chair queue. When an owner decision blocks
one issue, park that branch and choose independent authorised work. An empty
queue is a waiting condition for a standing mission, whose stop rule belongs
to `session`.
