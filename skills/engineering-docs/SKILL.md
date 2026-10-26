---
name: engineering-docs
description: "Use for creating, placing, indexing, updating, or archiving engineering docs and docs structures. Not for prose-only rewriting or session continuity; use engineering-writing or session."
---

# Engineering docs

Treat docs as deliverables. Load engineering-writing for substantial prose;
this skill owns placement and decision lifecycle.

Read `Repository process` first. If absent, use an unambiguous existing owner
or propose paths; never invent authority. In `issue-tracker`, parent issues own
change scope, stories and current status. Docs own durable intent, decisions
and invariants. In `project-docs`, use its named home. Do not create a second
story or status projection.
Link-only files are unavailable or cross-tracker fallbacks.

## Default homes

Use these only with setup-repo write authority; advisory mode proposes paths.

| Type | Home | Convention |
|---|---|---|
| Invariant specifications | `docs/specs/` | non-derivable intent; semantic paths |
| Generated reference | `docs/reference/` | code-derived inventories; generator command on first line |
| Architectural decisions | `docs/adr/` | current decisions; indexed |
| Architecture map | `docs/ARCHITECTURE.md` | current boundaries; link ADRs |
| Runbooks | `docs/runbooks/` | operator steps and verification |
| Open-decision register | declared project-docs path | never in issue-tracker mode |
| Diagrams | owning doc or `docs/diagrams/` | separate only for distinct ownership or reuse |
| Threat models | `docs/threat-models/` | STRIDE/LINDDUN |
| Continuity / archive | declared owner / `docs/archive/` | session owns handoff lifecycle; index archives |

## Binding rules

- **Link or generate, do not restate.** Each domain keeps its natural owner;
  link code, schemas, tests and other docs. Regenerate derived reference, never
  hand-edit it. Specs retain intent, prohibitions and invariants.
- Specs and runbooks start with an `Owner:` line naming the responsible
  module or skill and an `Anchor:` line linking concrete source symbols,
  tests or governing sections. Recheck anchors on change. See
  [placement](references/placement.md) for boundaries.
- Verify cited decision IDs and repair indexes and inbound links together.
  Keep ADR IDs stable; gaps left by retirement are deliberate.
  Keep other numbered directories contiguous.
- Quote and link frozen legal, compliance or gate wording.
- Review bloat by owner, audience and lifecycle. Merge duplicate truths and
  tiny files changed together; make claim → owner → evidence reachable in
  three hops. Session residue belongs in session.
  A doc past about 15 KB signals review, not an automatic split.

## Living ADRs

Record only architecture: crate/module and trust boundaries, protocols,
storage and major dependencies. Amend accepted ADRs in place to state the
current decision, adding one line `Changed: YYYY-MM-DD, #issue`. Git keeps
history. Merge or delete superseded ADRs and fix inbound links; index live
decisions only. Move process into its skill or constitution, and deferrals
or product calls into their issues. Do not leave compatibility stubs.

## Instruction files

The root file routes. Add nested files only at real boundaries, after an
observed failure; keep each 10 to 40 lines. `AGENTS.md` is canonical and each
`CLAUDE.md` contains exactly `@AGENTS.md` on one line. Re-check placement
and loading on model upgrades. Do not infer loading behaviour from filenames.

## Diagrams and retirement

Use Mermaid by default; apply [diagram quality](references/diagram-quality.md),
render and visually inspect before commit. Use d2-diagrams for justified fixed
layouts. Archive retired substantive docs with owner authority, index them and
repair references. Outside ADRs, keep a tombstone only for an external link or
unsupported consumer that needs it. ADR retirement follows the living policy
above.
