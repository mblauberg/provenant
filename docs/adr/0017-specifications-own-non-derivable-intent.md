# ADR 0017 — Specifications own non-derivable intent only

**Status:** Accepted 2026-07-28 (user); amends [ADR
0009](0009-standalone-semantic-specifications.md); applies [ADR
0011](0011-github-owns-work-state.md) and [ADR
0004](0004-per-domain-truth-owners.md)

## Context

The specification corpus reached 34 documents and 18,102 lines. Roughly 4,701 of
those lines are SQL DDL spread across nine files, describing the same schema that
`runtime/agent-fabric/migrations/0001-current-baseline.sql` already defines in
10,797 authoritative lines.

Nothing binds the two. `database-baseline.mjs` reads, executes and hashes only
the migration; `migrations.ts` rejects a hash mismatch against the migration; no
generator emits spec SQL and no check compares them.

The corpus drifted accordingly. Comparing column sets across the 86 tables the
two sources share, 19 disagree and 67 agree exactly. The worst are not marginal:
`review_portal_process_custody` carries 44 columns in the specification that the
migration does not have, and `provider_actions` is missing 25 that it does. Six
further SQL objects the fixtures name exist nowhere in the migration under any
similar name, and a 431-line block of `review-custody.md` narrates thirteen
review-portal tables of which twelve appear nowhere outside `docs/`.

None of this is caught, because nothing compares the two artefacts.

This is the failure ADR 0004 predicted. A domain grew a second source of truth
beside its natural owner, and no drift check stood between them.

Two existing decisions already cover most of the remedy:

- ADR 0004 gives each domain one truth owner that generates or validates its own
  projections. The migration is the schema's natural owner.
- ADR 0011 requires specifications to own requirements and acceptance criteria
  "without reporting implementation or verification state". Present-indicative
  delivery narration in the corpus already violates it.

What neither decision states is the positive rule about specification *content*:
that restating a structure the code already owns is itself the defect, however
accurate the restatement is on the day it is written.

## Decision

A specification owns only what code cannot express and tests cannot prove.

Ownership across the tree is:

| Question | Owner |
|---|---|
| What exists | code, schemas, registries, `0001-current-baseline.sql` |
| That it behaves | the behavioural test suites |
| What is delivered, and by whom | GitHub issues and the Project Status field |
| Why the architecture is this way | ADRs |
| What was required, and what must never happen | specifications |

Concretely:

- Specifications carry no DDL, no schema listings, no field or column
  inventories, and no structural restatement of implemented code.
- Specifications retain non-derivable normative intent: negative and must-deny
  requirements, ordering and concurrency constraints, digest preimage
  definitions, intended failure semantics, security boundaries and explicit
  requirement matrices.
- Any structure a specification does still restate is gated against its owner.
  `scripts/check_spec_schema_drift.py` ratchets table presence, column sets and
  ordered primary-key, unique and foreign-key signatures. Excluded schema
  properties and surrounding prose remain review-owned.
- A requirement that is normative but unimplemented is an open GitHub issue, not
  a present-tense sentence in a specification.
- `docs/invariants/agent-fabric.md` is the retention pattern: a durable claim, the
  mechanism that enforces it, and the test that evidences it.

## Consequences

The corpus loses the covered class of drift between prose schema and shipped
schema, because the duplicate is removed where it is pure restatement and gated
where it survives. Excluded schema properties and surrounding prose still need
review against their owners.

Readers lose the convenience of a prose tour of the schema sitting beside its
narrative, and must open the migration or the generated catalogue for concrete
tables, columns and indexes. That cost is accepted because those artefacts are
already the only authoritative ones.

The behavioural fixtures are deliberately **not** repointed at the migration.
Investigation found they are a semantic re-modelling rather than a failed copy:
they use a different timestamp representation, define fixture-local alias
relations, and build reduced support schemas around the objects under test.
Replaying their inserts against the baseline fails at 98%, and two negative
assertions were shown to pass against the baseline for the wrong reason, on an
unrelated foreign key. Repointing them would destroy a working oracle and
manufacture false confidence rather than remove it. The drift gate gives the
protection the repoint was meant to give, without that cost.

Deleting a restatement therefore requires knowing that it *is* one. Identifier
absence alone does not establish that a specification describes something
unbuilt, because this codebase has renamed objects without renaming their
specifications: of 34 triggers the specifications name, 5 match exactly, 18 exist
under another name, and 11 are genuinely absent. Removal needs rename-aware
evidence, and a stale name is corrected rather than deleted.

ADR 0009 is amended, not superseded. Its decision on semantic paths, independent
subject files, line limits and Git-owned integrity stands unchanged, as does its
consequence that behavioural fixtures read their owning specifications. This ADR
narrows what those specifications may contain; it does not move where the
fixtures read from.

ADR 0011 is unchanged and unsuperseded. Removing delivery narration from
specifications is compliance with it rather than a new decision.

The alternative of moving the whole corpus into GitHub issues was rejected. It
would relocate the duplication rather than remove it, and it would contradict
both ADR 0009's independent subject files and ADR 0011's assignment of
requirements ownership to specifications. Negative security requirements,
ordering constraints and digest preimages need a durable requirements owner that
a closed issue does not provide.
