# ADR 0009 — Standalone semantic specifications

**Status:** Accepted 2026-07-15 (human direction; supersedes the earlier
family-manifest form of this ADR); amended 2026-07-28 by [ADR
0017](0017-specifications-own-non-derivable-intent.md), which narrows what these
specifications may contain and aligns their checks with current documentation
tiers. Its 2026-08-31 issue-native scope amendment is noted below as consistent.

ADR 0017's 2026-08-31 issue-native scope amendment is consistent with this
decision: tracker issues or the declared project-docs home carry change scope
and stories, while these semantic files retain durable non-derivable intent.

## Context

The large specification monoliths needed to be split by durable ownership.
An intermediate design made each monolith a family with an ordered manifest,
mirrored versions, per-file hashes and a dedicated acceptance map. That design
duplicated guarantees Git already provides and coupled otherwise independent
documents through routine hash and ordering updates.

The replacement model uses one current owner per claim, unnumbered domain/topic
paths, and Git as the owner of source history. The human confirmed that
compatibility with the numbered monoliths is unnecessary and that the split
should not create new numbering or cross-reference dependencies.

## Decision

Normative specifications use semantic paths. At adoption, the layout was:

```text
docs/specs/
  README.md
  agent-fabric/<subject>.md
  console/<subject>.md
  harness/<subject>.md
```

[`docs/specs/README.md`](../specs/README.md) is the discovery index. Directories
retired with their runtimes are historical, not current specification homes.

Each subject file is an independent normative specification. Directories and
`docs/specs/README.md` exist only for discovery; they do not impose a parent
contract, load order, version mirror or acceptance state.

- One file owns each durable behaviour and each normative requirement ID.
- Durable requirements and normative acceptance criteria stay with their specification.
- Filenames and links use stable semantic names, never positional numbering.
- Git commits own byte integrity and history. Checked-in content hashes,
  ordered manifests and synthetic family concatenation are not maintained.
- No monolith copy, redirect, compatibility alias or old-number anchor survives
  the cutover.
- A normative specification is limited to 999 lines and 100 KiB. New ownership
  is split into another semantic specification instead of packing content to
  meet a fixed module count.
- Owner-specific tests gate machine-critical projections. Specification prose
  and reasoning remain review-owned; there is no universal specification
  checker or permanent checker over the whole corpus.
- Source-equivalence maps and frozen-block comparisons are one-time migration
  evidence, not permanent runtime or authoring machinery.

If a future release or external consumer genuinely needs a cryptographic spec
inventory, it is generated from the accepted Git commit as release evidence.
It is not hand-maintained beside the source.

## Consequences

Specification topics can change independently without refreshing unrelated
hashes, root metadata or ordered lists. Reviewers can open the one owner linked
from an issue or pull request. The repository loses a canonical concatenation
order, but no current runtime consumer requires one.

Existing behavioural fixtures read their specific owning specs or an explicit
test-only set of owners; no production helper reconstructs a pseudo-monolith.
Machine-critical projections stay with their owner-specific tests, while prose
and reasoning remain review-owned.
