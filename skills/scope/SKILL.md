---
name: scope
description: "Use when work is new or unsettled and needs a spec: acceptance criteria, risk tier, authority, one-way-door approval, a grill-me interview, a feasibility spike or a kept prototype. Not for implementing approved scope; use implement."
---

# Scope

Scope is the front human gate. Humans spend their effort here; after approval the
agents work without them. A good spec lets a fresh agent build and verify the work
without asking.

## Spec gate

Produce a one-screen digest and ask for one approval:

- outcome, and the number of criteria
- risk tier and authority (paths, effects, expiry)
- one-way doors: release, publish, send, spend, destroy, widening authority
- assumptions made, and open calls with a recommended default for each

Only the user can downgrade risk. Approval covers the digest; a changed criterion
needs a new revision and a new approval. Implementers cannot weaken criteria.

## Size to the class

A bug is repro, expected, observed and a regression check. A task has 2 to 5
criteria, a feature about 10. Anything larger is an epic: a `Done when` plus
backlog lines, not a mega-spec. Over-specifying a small change is a defect.

State what and why, plus one-way-door constraints. Leave design to the implementer
unless a decision has a rejected alternative, which becomes an ADR.

Write each criterion as one observable statement, `When <event>, <behaviour>`,
tagged with how it is verified: `[test]`, `[command]`, `[capture]` or `[review]`.
Add must-not-change behaviour and the verify command. The readiness test: could a
fresh agent build and verify this without asking? If not, it is not ready.

## Questions

Ask only when uncertainty and stakes are both high. The ladder:

1. Engineering call: decide and record `Decided (<source>): <reason>`.
2. Reversible owner call: assume, record, flag in the digest.
3. Irreversible owner call: `provenant ask --file Q.json --session S` with options and
   a recommendation (file shape in the human-inbox runbook). Park only that
   branch; continue the rest.

Never leave a `Spec gap:` line or a guess.

## Modes

- **Interview** (the user asks to be grilled): one question at a time, highest
  leverage first, each with evidence and a recommendation. Answer from the
  workspace instead of asking. Stop when criteria are clear.
- **Spike**: one falsifiable question, a timebox, kill criteria, a registered scratch
  worktree. Harvest the answer into the spec and discard the code. Read
  [interview and spike](references/interview-and-spike.md).
- **Kept prototype**: when it is a reference, own the repo file and index it
  through engineering-docs. Publishing a private Claude artifact copy is fine; the
  repo file stays the source. For UI, see ui-ux-design.

## Fan-out

Name the preset, then run it with `--explain` first:

```sh
provenant run --preset scout-swarm --prompt-file brief.md --set 'parameters.external=1'
provenant run --preset fan-out-synthesise --prompt-file options.md --set 'parameters.width=3'
provenant run --preset scope-then-run --prompt-file brief.md --params-file lane.toml --explain
```

- `scout-swarm` maps the ground; `fan-out-synthesise` compares options with no oracle.
- `scope-then-run` goes on to a build, so use it only when the build is authorised.
  Its `plan.scope` node ends `VERDICT: PASS|FAIL` and gates the writer.
- A read-only `scope-only` preset (scout, `plan.scope`, ready gate) is proposed.

The role briefs in `references/` (scout-areas, researcher, architect,
acceptance-writer, critic) are loaded by those presets. Do not rename them.

## Land

The spec lives in the issue body and dies with the delivering PR; tracker owns the
template. Cite `path#Symbol`, never scratch files.
