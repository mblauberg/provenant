# Document ownership and placement

Link or generate, do not restate. A domain's natural owner generates or
validates its projections; do not introduce a universal manifest as another
source of truth.

| Content | Owner / placement |
|---|---|
| Implemented fields, schemas, flags, defaults, command/tool catalogues | Code and registries; link them or generate reference in `docs/reference/` |
| Generated reference | Name the generator command on line one; regenerate and check diffs |
| Non-derivable intent, negative requirements, ordering, failure semantics, digest preimages and security invariants | `docs/specs/`; link tests without copying their assertions |
| Architectural rationale and trade-offs | Live ADR, amended in place |
| Process and agent behaviour | Governing skill or `HARNESS.md` section |
| Product calls, deferrals and unimplemented requirements | Owning issue; use the declared project-docs home where applicable |
| Current owner, scope, dependencies, delivery evidence and workflow state | Declared tracker or project-docs owner, never a parallel docs status table |
| Operational steps | Runbook, linked to its source and verification |
| What changed and when | Git history; curated release fragments follow repository release policy |

Specs use independent semantic topic paths, indexed for discovery. No directory
imposes load order, mirrored versions or a parent contract. One owner holds each
requirement. Git owns integrity and history; do not maintain hashes or synthetic
concatenation beside source. Keep a spec below 999 lines and 100 KiB by splitting
at an actual ownership boundary.

Each spec and runbook starts beneath its title with two lines:

- `Owner:` names the responsible crate/module, skill or declared document owner.
- `Anchor:` links concrete source symbols, tests or governing document sections
  sufficient to check its claims. Use relative source links and name the symbol
  or test, so a line-number shift does not hide its identity.

Mechanical owner, anchor, link and generated-diff checks may fail CI. Whether
prose belongs in a spec, expresses the right invariant or needs shortening is
review judgement, advisory to CI.

Before cutting apparent restatement, check renames and live callers. Identifier
absence alone does not prove the requirement obsolete. If a machine-critical
projection must remain, name its code owner and drift check; other prose stays
review-owned. Never move the whole durable corpus into issues merely to relocate
duplication.

Frozen source-equivalence maps and comparison inventories
are migration evidence, not permanent authoring machinery.
