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

A plan, or a reply to a proposal, names the structural option alongside the
small one, recommends one and says when an idea is weak or a better one exists,
because agent-written code is cheap to change and the small option is not the
default.

Show `provenant tracker lint N` advice before starting an issue unless already
supplied; unavailable checks do not block work.

Read repository checks, commit rules and review floors in `AGENTS.md` and
runbooks. Run targeted checks locally; leave full suites to CI.

## How it runs

Save `slice.toml`, replacing the scope and brief. `run --branch NAME --base REF` creates the writer's owned worktree, so register none by hand:

```toml
use = "implement"
tier = "junior"
write_scope = ["src"]
brief = "Meet the approved criteria; run the named checks, commit and report."
```

Run `provenant run --route junior --file slice.toml --branch NAME --base REF --explain`, then omit
`--explain` to submit.

Check authority, write scopes and gates in the explanation,
then submit without `--explain`. `deliver` is the default composite; use
`implement` for one slice, `implement.fix` for a known defect, and `batch` for
parallel slices into one integration branch. Use `plan` when the shape is open.
Inspect available templates with `provenant templates` and their definitions
with `provenant templates NAME --json`.

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
within approved scope: the principal records `repair_design` booleans
`within_scope` and `one_way_door` in its findings block. The host notifies the
owner and bypasses its `decide` wait only for explicit approved scope with no
one-way door. Changed scope, one-way doors and missing assessments wait on that
host decision before any continuation writer. Approval retains the original
literal write authority. A failed replanned panel feeds its qualified findings
into the remaining repair cycles. If no separate route is available, continue in a fresh
attempt on the available route with a recorded separation warning.

## Chair-run review loops

When you run review and repair by hand, judge every finding against the code
before any writer starts, because a lens can be wrong and a writer patches what
it is given. Mark each one confirmed or rejected with a reason, and worth fixing
or not. Rejected findings stay in the record and out of the writer's brief.
Group confirmed findings by shared cause into fix items, so one flaw is fixed
once. State the invariant under repair in the repair brief and the delta review,
not only the files, so a twin copy elsewhere is caught. You may overrule a
changes verdict after checking the code, with the reason on the pull request;
a dispute touching a hard boundary or data loss needs a test or a second lens.

After a shared flaw or a third round, ask whether the design is wrong, comparing
the structural fix with the small one and recommending one. Round count prompts
that judgement and does not end the work: if the design holds, keep repairing.

## Writer rules

- The implementer may not edit a failing acceptance test or weaken an
  assertion to pass. A changed test is reported with the reason.
- Update the owned docs in the same PR: behaviour, architecture, operations. A
  settled decision moves to its durable home (see `engineering-docs`).
- Keep compatibility only for published contracts. Otherwise change the
  callers and delete the old path; when persisted state changes, ship a
  migration or drain.
- Report checks that did not run as unverified, never as passing.
- A repair that leaves failures unchanged or larger twice in a row stops the
  run.
- When the spec is wrong, at any point and not only after failed repairs, cite
  the evidence in handoff `decisions` and take the smallest option inside write
  scope that serves the stated outcome, so the flaw is raised when found. If no
  such option exists, return the evidence to `scope`.

## Done

Exact-head CI and the review the tier requires meet acceptance. Finish with
the outcome report owned by `deliver`. Promotion needs separate authority
(`release`).
