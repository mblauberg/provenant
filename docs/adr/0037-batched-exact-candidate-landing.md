# ADR 0037: Batched exact-candidate landing

**Status:** Accepted

Changed: 2026-11-05, #2856.

## Context

Checking each pull request head does not check its combination with the target
at publication. Requiring every branch to be updated first serialises even
independent changes and repeats CI. Review and lineage also refer to approved
commit SHAs, so the landing history must preserve or map them.

## Decision

Use a batched candidate by default, including an unflagged single PR under
`integration = "batch"`. `--wait`, `--head` and `integration = "single"`
retain the single-PR path. From the fetched target, chain one merge
commit per eligible pull request in order, with the approved head as its second
parent. Gate the final candidate's exact commit and tree before publication;
recheck heads and approvals and rebuild and re-gate when the target moves. No
pull request branch needs updating. A red batch splits into ordered prefixes; a
conflict isolates the conflicting pull request after the clean prefix is gated.

`auto` publication selects `fast_forward` when the landing identity can advance
the target directly, and `rest_ordered` otherwise. The former advances from the
expected base without force; the latter merges pinned heads in order through the
forge after checking the expected target before each merge. A target change can
still race that check and the merge; the resulting-tree comparison alarms as a
backstop. `method` applies only to `rest_ordered`. Forge rules and
required checks remain in force. A publishing marker precedes publication;
once the outcome is definite it becomes a settled marker with the outcomes and
remaining follow-up steps. Only a publishing marker can authorise a retry.
Recovery completes the settled steps and deletes the marker last. A moved target
consumes a rebuild. Ambiguous responses leave a publishing marker until ancestry
and forge state prove the outcome. Recovery of a landing completes the approval
recheck, post-merge follow, receipt, tree comparison and candidate-ref cleanup.
A lease push still in flight after the configured settle window can land after
recovery concludes it did not land and clears the marker, leaving no receipt.
The window bounds this residual race; setting it to zero removes that wait.

Every PR receives an outcome receipt, including in shadow; shadow records the
selected publication mode, reason and split. Receipts retain policy sources,
candidate and gate evidence, publication,
outcomes, cost and alarms, including a mapping from approved to landed SHAs for
methods that rewrite them. A configurable fast local pre-push gate catches cheap
failures early. Post-merge checks can bisect a failed batch and alarm; automatic
revert-forward requires explicit configuration. It creates a mainline-parent revert on the current target, pushes an expected-absent revert branch and opens a PR naming the culprit and failed checks; failures raise an alarm naming the revert branch and instructing a manual retry; revert creation is not retried automatically. Batch errors retain prior PR outcomes in the result envelope and use distinct codes for contention, conflict, unmet gates, unknown publication and spending caps. Pool rates and durations inform
cost estimates: an advisory amount warns, while an explicit cap refuses excess
priced spend only; unknown cost never refuses. `gate.pool` and
`post_merge.pool` name cost pools, with the check issuer app slug as a checks
gate fallback. Post-merge follow polls until checks settle or its timeout raises
an `unsettled` alarm. Command gates scrub forge token environment variables
unless explicitly passed through. Candidate CI must trigger on the configured
ref prefix.

## Alternatives and consequences

Always updating branches was rejected because disjoint changes would incur
serial churn. A forge merge queue as the default would require per-group builds
and depend on the forge's queue. Squash as the default would orphan reviewed
SHAs; merge commits preserve them. Other methods require the receipt mapping.
The candidate gate adds an integration build, but it checks the combination
intended for publication. Ordered REST publication must verify the resulting
tree because the forge creates the merge commits.

## Addendum: review receipts as approval

`land.approval = "receipt"` lets an accepted Provenant delivery receipt stand in
place of an approving forge review, through one approval port beside the
forge's. The receipt's pull request subject records the reviewed head `H` and
base `B`, which the save reads from the forge as the head's merge base with the
branch the pull request merges into; for a stack member `B` is the head of the
branch below. Receipts are matched by repository and pull request number. A head `H'`
keeps the review when `H' == H`, or when, with `B'` the merge base of `H'` and
the fetched target, `B` is an ancestor of `B'`, `B'` is an ancestor of the
target and `tree(H')` equals `merge_tree(H, B')`. Selection, revalidation
before and during publication, recovery and the post-merge approval recheck all
apply this proof against the target they already read; a head that fails it is
excluded and never merged. A record whose proof cannot be read excludes only
its pull request, as unprovable. A receipt without a base covers only its own head.
The graph land node applies the same rule to the head its verdict gate reviewed,
against the branch the land branch's pull request merges into.
Pinned heads, markers, recovery, the tree comparison and forge-required checks
are unchanged. The second condition holds by construction of `B'`; it stays so
the rule reads the same wherever `B'` comes from.
