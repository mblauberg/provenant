---
name: engineering-docs
description: "Use to decide whether and where a spec, ADR, runbook, instruction file or diagram lives."
---

# Engineering docs

This skill decides which docs are worth keeping and where they live; `writing`
shapes the prose. Read the repository process declaration first; if it is
absent, use an unambiguous existing owner or propose paths. With an issue
tracker home, the issue owns scope and status, and docs own durable intent and
invariants with no second status projection.

## Durable or ephemeral

| Kind | Fate |
|---|---|
| Issue-body spec, plan, handoff, scratch notes | Ephemeral. Dies at delivery; its settled rules move into a durable doc in the same pull request |
| Invariants, non-derivable intent, ordering, failure semantics | Durable spec |
| Architecture: crate and trust boundaries, protocols, storage, major dependencies | Living ADR |
| Operator steps | Runbook, linked to its source and verification |
| Implemented fields, flags, schemas, commands | Code, or generated reference. Never hand-edit it |
| Other settled decisions | The doc they change (spec, runbook, skill), or the thread if ephemeral |
| What changed and when | `git log`; release fragments per repository policy |

Prefer generated reference for facts code can provide; retain intent and link its owners.

## Rules

- **Edit in place.** A living doc states current truth. Rewrite or delete
  superseded text in the same change; never append dated `Update:` or "as of"
  entries. Changelogs and release notes are the exception.
- **Owner and anchor.** Each spec and runbook opens with `Owner:` and `Anchor:`
  naming owning modules, types or declarations. Specs never name individual
  tests, line numbers or inventory counts. Tests cite the section they pin with
  `Spec: docs/specs/NAME.md#anchor`; the drift gate resolves these links.
  A rule's `Why:` cites its event, or reads
  `Why (default):` for a default open to change.
- Link claims to owners and evidence. Merge duplicate truths and
  tiny files that change together. Before cutting apparent restatement, check
  renames and live callers. Repair indexes and inbound links together.

## ADRs

Architecture only: the decision, its forces and consequences. Add
`Overturn when: <fact>` only for a checkable fact. Amend an accepted ADR in
place, keeping one `Changed: YYYY-MM-DD, #issue` line that each amendment
replaces. Merge or delete superseded ADRs and leave no compatibility stubs.

## Prototypes, instructions and diagrams

A kept prototype lives in the repository with a one-line index entry in its
owning doc; throwaway ones are not indexed. The root instruction file routes;
by default a nested `AGENTS.md` is added only at a real boundary after an
observed failure, and `CLAUDE.md` holds just `@AGENTS.md`. Use `d2-diagrams` for diagram authoring and visual verification. Archive retired docs with owner authority and repair
references.

See [placement](references/placement.md) for default homes.
