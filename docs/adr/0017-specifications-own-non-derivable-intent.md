# ADR 0017 — Specifications own non-derivable intent only

**Status:** Accepted 2026-07-28 (user); amends [ADR
0009](0009-standalone-semantic-specifications.md); applies [ADR
0011](0011-github-owns-work-state.md) and [ADR
0004](0004-per-domain-truth-owners.md); amended 2026-08-31 (user, [issue
#711](https://github.com/mblauberg/provenant/issues/711)) for issue-native
change scope

> **Current-reader note.** The SQL and `runtime/agent-fabric` inventory below is
> historical after ADR 0020; the current Fabric schema owner is
> `runtime/fabric/schema.sql`. The live rule remains: specifications own
> non-derivable intent, while code and tests own implemented structure.

## Context

Before ADR 0020, the specification corpus reached 34 documents and 18,102
lines. Roughly 4,701 lines were SQL DDL spread across nine files, describing
the schema then defined by the 10,797-line
`runtime/agent-fabric/migrations/0001-current-baseline.sql`.

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

Two existing decisions covered most of the remedy:

- ADR 0004 gives each domain one truth owner that generates or validates its own
  projections. The migration was the legacy schema's natural owner; the current
  owner is `runtime/fabric/schema.sql`.
- ADR 0011 requires specifications to own durable requirements and normative acceptance criteria
  "without reporting implementation or verification state". Present-indicative
  delivery narration in the corpus already violates it.

What neither decision states is the positive rule about specification *content*:
that restating a structure the code already owns is itself the defect, however
accurate the restatement is on the day it is written.

Issue #711 exposes the adjacent ownership gap: a change scope or story needs
one declared home, but it must not displace durable requirements or decisions.
This repository declares its issue tracker as that home; repositories without
one may declare project docs instead. Git retains durable invariants and
history; the issue only carries current change scope and story.

## Decision

A specification owns only what code cannot express and tests cannot prove.

Ownership across the tree is:

| Question | Owner |
|---|---|
| What exists | code, schemas and registries; current Fabric: `runtime/fabric/schema.sql` |
| That it behaves | the behavioural test suites |
| What change scope, story and owner are current | the declared scope/story owner |
| What workflow phase is current | the declared workflow-state owner |
| Why the architecture is this way | ADRs |
| What was required, and what must never happen | specifications |

Concretely:

- Specifications carry no DDL, no schema listings, no field or column
  inventories, and no structural restatement of implemented code.
- Specifications retain non-derivable normative intent: negative and must-deny
  requirements, ordering and concurrency constraints, digest preimage
  definitions, intended failure semantics, security boundaries and explicit
  requirement matrices.
- Any machine-critical structure a specification still restates must be gated
  against its live owner; prose and reasoning remain review-owned. ADR 0020
  retired this ADR's legacy schema-drift checker with the old Fabric runtime;
  current structure is owned and tested under `runtime/fabric`.
- A requirement that is normative but unimplemented is an open work item in the
  declared scope/story owner, not a present-tense sentence in a specification.
- Retained structural claims name their code owner and drift test; current
  Fabric architecture is owned under `runtime/fabric`.

## Consequences

The corpus loses the covered class of drift between prose schema and shipped
schema, because the duplicate is removed where it is pure restatement and gated
where it survives. Excluded schema properties and surrounding prose still need
review against their owners.

Readers lose the convenience of a prose tour of the schema sitting beside its
narrative, and must open the live code owner for concrete tables, columns and
indexes. Current Fabric readers use `runtime/fabric/schema.sql`. That cost is
accepted because the live code owner is already authoritative.

The behavioural fixtures are deliberately **not** repointed at the migration.
Investigation found they are a semantic re-modelling rather than a failed copy:
they use a different timestamp representation, define fixture-local alias
relations, and build reduced support schemas around the objects under test.
Replaying their inserts against the baseline fails at 98%, and two negative
assertions were shown to pass against the baseline for the wrong reason, on an
unrelated foreign key. Repointing them would destroy a working oracle and
manufacture false confidence rather than remove it. At adoption, the now-retired
legacy drift gate supplied that protection without that cost; current Fabric
structure is code- and test-owned under ADR 0020.

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

ADR 0011 remains the current-work-state decision for this repository. Issue #711
adds the repository declaration that makes its issue-native scope/story route
explicit; it does not make project docs a second current-work owner.

The alternative of moving the whole corpus into tracker issues was rejected. It
would relocate the duplication rather than remove it, and it would contradict
ADR 0009's independent subject files. Issue-native change scope is limited to
the work item and story; negative security requirements, ordering constraints
and digest preimages retain durable specification ownership.
