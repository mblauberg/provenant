# ADR 0037: Batched exact-candidate landing

**Status:** Accepted

Changed: 2026-11-05, #2856.

## Context

Checking each pull request head does not check its combination with the target
at publication. Requiring every branch to be updated first serialises even
independent changes and repeats CI. Review and lineage also refer to approved
commit SHAs, so the landing history must preserve or map them.

## Decision

Use a batched candidate by default. From the fetched target, chain one merge
commit per eligible pull request in order, with the approved head as its second
parent. Gate the final candidate's exact commit and tree before publication;
recheck heads and approvals and rebuild and re-gate when the target moves. No
pull request branch needs updating. A red batch splits into ordered prefixes; a
conflict isolates the conflicting pull request after the clean prefix is gated.

`auto` publication selects `fast_forward` when the landing identity can advance
the target directly, and `rest_ordered` otherwise. The former advances from the
expected base without force; the latter merges pinned heads in order through the
forge and checks the resulting tree against the candidate. Forge rules and
required checks remain in force. An unknown outcome is reconciled before retry
or cleanup.

Receipts retain policy sources, candidate and gate evidence, publication,
outcomes, cost and alarms, including a mapping from approved to landed SHAs for
methods that rewrite them. A configurable fast local pre-push gate catches cheap
failures early. Post-merge checks can bisect a failed batch and alarm; automatic
revert-forward requires explicit configuration. Pool rates and durations inform
cost estimates: an advisory amount warns, while an explicit cap refuses excess
spend; unknown prices remain unknown.

## Alternatives and consequences

Always updating branches was rejected because disjoint changes would incur
serial churn. A forge merge queue as the default would require per-group builds
and depend on the forge's queue. Squash as the default would orphan reviewed
SHAs; merge commits preserve them. Other methods require the receipt mapping.
The candidate gate adds an integration build, but it checks the combination
intended for publication. Ordered REST publication must verify the resulting
tree because the forge creates the merge commits.
