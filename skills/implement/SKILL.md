---
name: implement
description: "Use to build an approved software change through the deliver loop: write, CI, repair, independent review, land. Not for unsettled scope (scope), root cause (diagnose), read-only review (code-review) or promotion (release)."
---

# Implement

Use `implement` for a slice and `deliver` for the full delivery loop.

## Entry

Start from an approved spec: outcome, acceptance criteria with how each is
verified, non-goals, risk tier and write authority (the tracker issue or the
scope digest). Missing inputs go back to `scope`. Do not invent owner decisions;
record an engineering call as `Decided (<source>): <reason>` and proceed.

Show `provenant tracker lint N` advice before starting an issue unless already
supplied; unavailable checks do not block work.

Read repository checks, commit rules and review floors in `AGENTS.md` and
runbooks. Run targeted checks locally; leave full suites to CI.

## How it runs

Register a writer worktree first (`provenant worktree create slice --base BASE`).
Save `slice.toml`, replacing the path, scope and brief:

```toml
use = "implement"
tier = "junior"
worktree = "/absolute/project/.worktrees/slice"
write_scope = ["src"]
brief = "Meet the approved criteria; run the named checks, commit and report."
```

Run `provenant run --route junior --file slice.toml --explain`, then omit
`--explain` to submit.

Check authority, write scopes and gates in the explanation,
then submit without `--explain`. `deliver` is the default composite; use
`implement` for one slice, `implement.fix` for a known defect, and `batch` for
parallel slices into one integration branch. Use `plan` when the shape is open.
See [templates](../../docs/runbooks/presets.md).

Follow `provenant status`, collect `provenant output TARGET`, and use `retry`
for unfinished work. `edit` changes unstarted nodes. Landing uses
`provenant land N --head SHA --wait` after the integrated head meets its gates.

## Writing the brief

The writer sees only the brief. Put in it the issue path, the acceptance
criteria, the files and owners already found, the checks to run, and the
instruction to stop and report instead of widening scope. One writer per
worktree. Name the method skill: `test-design` for observable behaviour,
`refactor` for restructuring, `diagnose` for an unknown cause.

A separate fresh writer attempt repairs qualified review findings; the raising
lenses re-review the delta. `max_repair_cycles` defaults to three and accepts
one to three, independently of the CI repair limit. A recurring finding or an
evidenced design defect escalates early to read-only principal scoping. Proceed
within approved scope and notify the owner; changed scope and one-way doors
wait on a `decide` node. If no separate route is available, continue in a fresh
attempt on the available route with a recorded separation warning.

## Writer rules

- The implementer may not edit a failing acceptance test or weaken an
  assertion to pass. A changed test is reported with the reason.
- Update the owned docs in the same PR: behaviour, architecture, operations. A
  settled decision moves to its durable home (see `engineering-docs`).
- Keep compatibility only for published or persisted contracts. Otherwise
  change the callers and delete the old path.
- Report checks that did not run as unverified, never as passing.
- A repair that leaves failures unchanged or larger twice in a row stops the
  run. Return the evidence to `scope` when the spec turns out wrong.

## Done

Exact-head CI and the review the tier requires meet acceptance. Finish with
the outcome report owned by `deliver`. Promotion needs separate authority
(`release`).
