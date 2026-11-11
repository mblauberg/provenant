# Default homes

Use these after `setup-repo`'s approved Declare-the-process step; otherwise propose paths.
Specs use independent semantic topic paths, indexed for discovery; no directory
imposes load order or mirrored versions, and one owner holds each requirement.
Write a spec as an invariants page: each entry states what must hold, with a
one-sentence Why, the test that pins it and its owner, while tests and generated
reference own behaviour detail; keep a page near 2,000 words, splitting at a real
ownership boundary.

| Type | Home | Convention |
|---|---|---|
| Invariant specifications | `docs/specs/` | non-derivable intent; semantic paths |
| Generated reference | `docs/reference/` | code-derived; generator command on line one |
| Architectural decisions | `docs/adr/` | current decisions; indexed |
| Architecture map | `docs/ARCHITECTURE.md` | current boundaries; link ADRs |
| Runbooks | `docs/runbooks/` | operator steps and verification |
| Diagrams | owning doc or `docs/diagrams/` | separate only for distinct ownership or reuse |
| Threat models | `docs/threat-models/` | STRIDE/LINDDUN |
| Kept prototypes | owning doc, indexed | repo file is the source |
| Continuity / archive | declared owner / `docs/archive/` | session owns handoff lifecycle; index archives |

If a machine-critical projection must remain, name its code owner and drift
check; other prose stays review-owned. Never move the whole durable corpus into
issues merely to relocate duplication.
