---
name: engineering-docs
description: "Use for creating, placing, indexing, updating, or archiving engineering docs and docs structures. Not for prose-only rewriting or session continuity; use engineering-writing or session."
---

# Engineering docs

Treat docs as deliverables: cite decisions, archive retired docs and load
`engineering-writing` for substantial prose.

Read `Repository process` first. If absent, use an unambiguous existing owner or
propose paths in chat; never invent one. In `issue-tracker`, parent issues own
canonical scope and stories; docs own durable intent, decisions and invariants.
In `project-docs`, use its named home. Do not create a second story or status
projection. Link-only files are unavailable or cross-tracker fallbacks.

## Default homes

Use this table only with `setup-repo` write authority; advisory mode proposes
paths without creating them.

| Type | Home | Convention |
|---|---|---|
| Durable intent / specifications | `docs/specs/` | semantic, indexed |
| Architecture map | `docs/ARCHITECTURE.md` | current state; link ADRs |
| Diagrams | owning document or `docs/diagrams/` | colocate unless independently owned, reused or generated |
| Runbooks | `docs/runbooks/` | numbered steps; verification |
| Open-decision register | declared project-docs path | never create in issue-tracker mode |
| Threat models | `docs/threat-models/` | STRIDE/LINDDUN structure |
| Session or run continuity | declared owner; explicit run state | `session` handoff; autopilot state remains run-local |
| Archive | `docs/archive/` | indexed by its README |

## Binding rules

1. Verify a decision ID against its log before citing it.
2. Regenerate generated files; never hand-edit them.
3. Keep numbered directories contiguous and update their index in the same
   change.
4. Quote and link frozen legal, compliance or gate wording; do not paraphrase.
5. **Anti-bloat**: a doc past ~15 KB signals review, not an automatic split.
   Split when owners, audiences, lifecycles or change rates
   differ; merge duplicate truths or tiny files changed together. Keep one
   owner and make claim → owner → evidence reachable in three hops. Session
   residue belongs in `session`'s context-hygiene pass.

## Diagrams

Default to **Mermaid** for GitHub and operational docs: use
`flowchart` for routing, `sequenceDiagram` for calls, `stateDiagram-v2` for
lifecycles and `erDiagram` for schemas. Colocate a load-bearing diagram with
its owner when both have the same audience, owner and lifecycle; separate it when reused,
generated or independently maintained.

**Render and visually inspect before commit.** Parser success proves syntax, not
layout quality:

```sh
src="$(pwd)/path/to/owning-document.md"
out="$(mktemp -d)"
(cd "$out" && mmdc -i "$src" -o check.md)
```

Keep one conceptual level; check normal and narrow widths for overlap, clipping,
crossings and unreadable scaling. Apply
[diagram-quality.md](references/diagram-quality.md); use `d2-diagrams` only
when fixed layout or publication quality justifies it.

## Retirement and archiving

Preserve durable records in the archive and index. After an authorised move,
repair inbound links and remove a pointer-only file when history and the index
preserve provenance. Keep a tombstone only while an external link or
unsupported consumer requires one. Deleting substantive records needs owner
authority.

## Red flags

- Unverified decision citation.
- Hand-edited generated output.
- Deleted history or an unindexed new document.
