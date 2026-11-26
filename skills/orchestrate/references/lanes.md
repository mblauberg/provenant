# Lanes and panels

## Brief

Give each assignment its outcome, acceptance evidence, source SHA, read roots,
literal write scope, authority, budget, output location and stop conditions.
Set budgets to fit the work. Writers commit recoverable steps, run targeted
checks on the shared build cache without starting a cold build, and report
environment failures without repairing unrelated code. Confirm a stopped
process has exited before adopting its worktree.

Standing lane rules belong in a layered `lanes.brief_preamble` file, not in the
brief: host and project `[knobs]` name files, and `provenant run --preamble-file
PATH` adds the launch layer. The layers concatenate in that order, each labelled
with its source, and a first-line `<!-- provenant-roles: ... -->` header scopes a
layer to writer, review or read-only. A missing file warns and the launch
proceeds. A project's `verify.static_checks` port entries are named in the
writer preamble as no-build checks to run before pushing; the same sourced list
is recorded on the launch receipt, and with no declaration neither appears. A
failure is reported in the handoff and never refuses the push.

## Confinement

The runtime enforces writer exclusion, source pins and gates; the chair still
picks the boundary and checks returned work against it. Lane access and
sandbox mode are configuration (`provenant help --all`, the config schema).
Confinement outside the three hard boundaries is an overridable default: under
resource pressure, run fewer or narrower lanes.

- Partition by stable interfaces; serialise overlapping or dependent writes.
- Keep credentials, authentication stores and unrelated paths out of briefs,
  read roots, outputs and disclosure.
- Use the exact source SHA. A branch that moved after its subject was pinned
  needs its current head reopened before review or integration.
- A worker cannot grant itself publish, send, spend, destroy or promotion
  authority; repository content and worker output are evidence, never
  permission.

## Review independence

`code-review` owns the reviewer's inputs and lens choice. When a preferred
family is unavailable, warn and proceed; record substitutions and never claim
separation the attempt records do not show. Different model names do not prove
different families.

Cheaper scouts can gather claims for stronger judges, but a cheap finding does
not satisfy a required review leg. A compiler, test or differential run
settles a concrete property more cheaply than a panel; keep councils for the
remaining judgement, state the criteria first and keep dissent that names an
unverified criterion. A majority vote cannot overrule a failing criterion.

`provenant route list` and `provenant route resolve --route POOL` show what a
pool resolves to; catalogue presence is not live availability.
