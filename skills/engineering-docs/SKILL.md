---
name: engineering-docs
description: "Use to create, place, index, update or archive engineering docs: specs, ADRs, runbooks, reference, instruction files, kept prototypes and diagrams. Decides durable vs ephemeral and where each belongs. Not for wording (engineering-writing) or session handoffs (session)."
---

# Engineering docs

This skill decides what docs are worth keeping and where they live;
`engineering-writing` shapes the prose.

Read the repository process declaration first. If it is absent, use an
unambiguous existing owner or propose paths; never invent authority. With an
`issue-tracker` home, the parent issue owns scope and status; docs own durable
intent and invariants, and no second status projection is created. A link-only
issue map is a fallback for unavailable or cross-tracker routes only.

## Durable or ephemeral

| Kind | Fate |
|---|---|
| Issue-body spec, plan, handoff, scratch notes | Ephemeral. Dies at delivery; its settled rules move into a durable doc in the same PR |
| Invariants, non-derivable intent, ordering, failure semantics | Durable spec |
| Architecture: crate and trust boundaries, protocols, storage, major dependencies | Living ADR |
| Operator steps | Runbook, linked to its source and verification |
| Implemented fields, flags, schemas, commands | Code, or generated reference. Never hand-edit it |
| Other settled decisions | The relevant doc (spec, runbook, skill). Ephemeral ones stay in the PR or issue thread |
| What changed and when | `git log`; release fragments per repository policy |

A spec that code can regenerate, or a task spec that has shipped, should die.
Keep only what code cannot say, and link code, schemas and tests.

## Binding rules

- **Edit in place.** A living doc states current truth. Rewrite or delete
  superseded text in the same change; never append dated `Update:` or "as of"
  entries to a spec, runbook, README or ADR. A diff that only adds lines to a
  living doc needs a reason. Changelogs and release notes are the exception.
- **Owner and anchor.** Each spec and runbook opens with `Owner:` (the
  responsible crate, skill or document) and `Anchor:` (relative links to source
  symbols or tests that let a reader check its claims). Name the symbol, not a
  line number.
- **Prevent drift.** Mechanical owner, anchor, link and generated-diff checks
  may fail CI; whether prose belongs in a spec is review judgement. Before
  cutting apparent restatement, check renames and live callers.
- **Three hops** from claim to owner to evidence. Merge duplicate truths and tiny
  files that change together. A doc past about 15 KB prompts review, not a split.
- Repair indexes and inbound links together. Quote and link frozen legal,
  compliance or gate wording.

## ADRs: architecture only

Record the decision, its forces and consequences, for architecture alone.
Process goes to its skill or `HARNESS.md`; deferrals and product calls to their
issues. Amend an accepted ADR in place to the current decision, keeping one
`Changed: YYYY-MM-DD, #issue` line that each amendment replaces. Merge or delete
superseded ADRs, fix inbound links, index live decisions only, and leave no
compatibility stubs.

## Kept prototypes

A prototype kept as reference lives in the repo with a one-line index entry in
its owning doc (what it proves, how to open it). It may be shared as a private
Claude artifact; the repo file stays the source. Throwaway ones are not indexed.

## Instruction files

The root file routes. Add a nested `AGENTS.md` (10 to 40 lines) only at a real
boundary after an observed failure. Each `CLAUDE.md` holds exactly `@AGENTS.md`.

## Diagrams and archive

Default to Mermaid; use `d2-diagrams` for a justified fixed layout. Render the
exact block and open the output: a clean parse is not visual verification.
Check overlap, clipping and lost failure paths, and run `mmdc` from a temporary
directory. Give each block `accTitle` and `accDescr`. Archive retired docs with
owner authority and repair references; keep a tombstone only for an external
link.

See [placement](references/placement.md) for default homes (they apply
after `setup-repo`'s approved Declare-the-process step; otherwise propose paths).
