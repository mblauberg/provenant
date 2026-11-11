---
name: scope
description: "Use to turn a rough or unsettled request into a spec, interview, spike or prototype."
---

# Scope

Scope settles intent and authority so a fresh agent can build and verify the
work without asking.

## Digest

Post a one-screen digest with `provenant ask` as recorded assumptions, then
proceed:

- outcome, and the number of criteria
- risk tier and authority (paths, effects, expiry)
- one-way doors: release, publish, send, spend, destroy, widening authority
- assumptions, and open calls with a recommended default for each
- the structural option beside the small one, and a recommendation

Block only on what the always-loaded rules reserve for the owner: a one-way
door, new authority, or acceptance you cannot infer. Park that branch and carry
on with the rest.

A spec is the best current statement of the outcome and can be wrong. An
implementer may not silently weaken it; it raises a flaw with evidence in the
handoff `decisions`. The chair may amend or drop an agent-written criterion on
that evidence by editing the spec and adding `Revised YYYY-MM-DD: <change>;
<evidence>`. Owner-written criteria, one-way doors, widened authority and
tightened acceptance still need the owner; when authorship is unclear, treat a
criterion as the owner's.

## Size to the class

A bug is repro, expected, observed and a regression check. A task has 2 to 5
criteria, a feature about 10. Anything larger is an epic: a `Done when` plus
backlog lines. Over-specifying a small change is a defect.

State what and why, plus one-way-door constraints, and leave design to the
implementer unless a decision has a rejected alternative. Write each criterion
as one observable statement, `When <event>, <behaviour>`, tagged `[test]`,
`[command]`, `[capture]` or `[review]`. Say how verification catches a wrong
implementation; test behaviour, not wording. Give each deletion or move one
owner. A verification step beyond the stated outcome names the failure it
prevents and how likely that is, or it is left out, because every later
implementer must honour it. Boundaries cite a recorded real boundary or their
reason.

## Modes

- **Interview** (the user asks to be grilled): one question at a time, highest
  leverage first, each with evidence and a recommendation. Answer from the
  workspace instead of asking.
- **Spike:** one falsifiable question, a timebox, kill criteria and a scratch
  worktree. Harvest the answer into the spec and discard the code. See
  [interview and spike](references/interview-and-spike.md).
- **Kept prototype:** the repository file is the source, indexed through
  `engineering-docs`; a private artifact copy may present it.

Shape open work with the `plan`, `explore`, `research` and `design` templates;
the briefs in `references/` supply scouting, research, architecture,
acceptance and critique questions. Read-only scoping grants no build authority.

## Home

The spec lives where the repository's process says: by default the issue body
(`tracker`), which dies with the delivering pull request. Cite `path#Symbol`,
never scratch files.
