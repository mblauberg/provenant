---
name: engineering-docs
description: "Use for creating, placing, indexing, updating, or archiving engineering docs and docs structures. Not for prose-only rewriting or session continuity; use engineering-writing or session."
---

# Engineering docs

Treat docs as durable, audited deliverables. Cite decisions, archive retired
docs, use Australian English and load `engineering-writing` for substantial
prose.

Read the repository's `Repository process` declaration before choosing a home.
When it declares `issue-tracker`, the parent issue owns the canonical scope and
stories; project docs own durable intent, decisions and invariants. With
`project-docs`, use its named scope/story home. Do not create a second story or
status projection. A link-only file is only a fallback for an unavailable or
cross-tracker route.

## Default homes

Resolve project instructions and existing canonical owners first. The table is
a fallback only when project-write authority includes documentation setup. In
advisory mode, propose paths without creating them.

| Type | Home | Convention |
|---|---|---|
| Durable intent / specifications | `docs/specs/` | semantic path, indexed |
| Architecture map | `docs/ARCHITECTURE.md` | current state; links to ADRs |
| Diagrams | owning document or `docs/diagrams/` | colocate by default; separate when independently owned, reused or generated |
| Runbooks | `docs/runbooks/` | numbered steps and verification |
| Open-decision register | `docs/OPEN_DECISIONS.md` | one row per user/owner gate; never auto-answered |
| Threat models | `docs/threat-models/` | STRIDE/LINDDUN structure |
| Session or run continuity | declared owner; explicit run state only | `session` handoff; autopilot run state remains run-local |
| Archive | `docs/archive/` | indexed by its README |

## Binding rules

1. Verify a decision ID against its log before citing it.
2. Regenerate generated files; never hand-edit them.
3. Keep numbered directories contiguous and update their index in the same
   change.
4. Quote and link frozen legal, compliance or gate wording; do not paraphrase.
5. **Anti-bloat**: an agent-facing doc past ~15 KB is a split/merge review
   signal, not an automatic split. Split when owners, audiences, lifecycles or
   change rates differ; merge duplicate truths or tiny files always changed
   together. Keep one canonical owner and make current claim → owner → evidence
   reachable in at most three hops. Session residue never accumulates in live
   dirs; use `session`'s context-hygiene pass.

## Diagrams

Default to **Mermaid in markdown** for GitHub and operational docs. Use
`flowchart` for routing, `sequenceDiagram` for calls, `stateDiagram-v2` for
lifecycles, `erDiagram` for schemas and a C4-style flowchart for context.

Place a load-bearing diagram in the document whose explanation it supports
when both have the same audience, owner and lifecycle. Use a separate diagram
file when it is reused, generated, or maintained as an independent artifact.

**Render and visually inspect before commit.** Parser success proves syntax,
not layout quality:

```sh
src="$(pwd)/path/to/owning-document.md"
out="$(mktemp -d)"
(cd "$out" && mmdc -i "$src" -o check.md)
```

Keep one conceptual level per diagram. Split overview from detail when return
edges distort the main path. Check normal and narrow widths for overlap,
clipping, crossings, blank space and unreadable scaling. Apply
[diagram-quality.md](references/diagram-quality.md). Use `d2-diagrams` only
when fixed layout or publication quality justifies a rendered asset.

## Retirement and archiving

Preserve durable records in the archive and update its index. After an
authorised move, repair inbound links and remove a pointer-only file when
repository history and the relevant index preserve provenance. Keep a
tombstone only while a stable external link or unsupported consumer still
requires one. Deleting a substantive record still requires owner authority.

## Red flags

- Unverified decision citation.
- Hand-edited generated output.
- Deleted history or an unindexed new document.
