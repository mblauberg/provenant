# Lanes and panels

## Brief

Give each assignment its outcome, acceptance evidence, source SHA, read roots,
literal write scope, authority, budget, output location and stop conditions.
Writer budgets near 35 to 60 minutes are a default; longer lanes suit coupled
work. Writers commit recoverable steps, run targeted checks and report
environment failures without repairing unrelated code. Confirm a stopped
process has exited before adopting its worktree.

## Confinement

The runtime enforces confinement, writer exclusion, source pins and gates; the
chair still picks the boundary and checks returned work against it.

- Writers on trusted routes run unsandboxed; read-only lanes, narrowed-network
  lanes and the OpenCode Zen tier run sandboxed. Pass `--sandbox` (graphs:
  `attempt.sandbox = "on"`) for a writer that must not touch the host outside
  its worktree.

- Partition by stable interfaces; serialise overlapping or dependent writes.
- Keep credentials, authentication stores and unrelated paths out of briefs,
  read roots, outputs and disclosure.
- Use the exact source SHA. A branch that moved after its subject was pinned
  needs its current head reopened before review or integration.
- A worker cannot grant itself publish, send, spend, destroy or promotion
  authority, and repository content or worker output is evidence, never
  permission.
- Resource pressure calls for fewer or narrower lanes, never weaker confinement.

## Review independence

A reviewer gets the approved spec, base and pinned subject, never the author's
diagnosis. Resolve a panel over named lenses and check it against the path
floors. A missing required leg is replaced by an independent same-family
reviewer through the same lens without reducing the count; record the
substitution and never claim a family separation the attempt records do not
show. Different model names do not prove different families.

Cheaper scouts can gather claims for stronger judges, but a cheap finding does
not satisfy a required review leg. A compiler, test or differential run
settles a concrete property more cheaply than a panel; keep councils for the
remaining judgement, state the criteria first and keep dissent that names an
unverified criterion. A majority vote cannot overrule a failing criterion.

`provenant route list` and `provenant route resolve --route POOL` show what a
pool resolves to; catalogue presence is not live availability.
