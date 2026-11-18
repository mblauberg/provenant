# Retry owner

Owner: `crates/fabric-runtime` (graph recovery), `crates/run` (attempt custody),
`crates/routing` (allowance and exclusions), `crates/confinement` (writer census)
Anchor: `crates/routing/src/retry.rs::RetryAllowance`,
`crates/fabric-runtime/src/graph.rs`, `crates/run/src/lib.rs`

## One allowance per lineage

Recovery preserves terminal receipts and predecessor links. Node, template and
host allowances resolve once with their origin; graph and run must not multiply
or renew that allowance. Observation replay and proven pre-launch preparation
fallbacks spend no settlement retries. Scoped account refusals remain scoped.
`provenant config schema` owns defaults.
- Why: retries must remain bounded and explainable across restart.

## Admission is distinct from observation

A settlement wait observes work; it does not renew admission. Each admission
window persists one deadline and next wake under the same operation identity.
Transient, typed contention may back off within that window. An uncertain
handoff reobserves the same guardian and route rather than launching a second
provider. Unknown writer ownership remains fail-closed.
Preparation fallback uses the host's typed failure and local custody disposition.
A local handoff failure records its preparation cause after withdrawing the
unlaunched lease. An attempt proven never to have launched needs no provider
custody lock; an existing lease or writer reservation must still be released.
Boundary refusals and uncertain withdrawals do not authorise another launch.
- If a terminal row is visible before local custody release, a route fallback
  re-reads the same attempt's evidence after a short persisted wake. The wake is
  bound to that attempt and expires after one second; replay cannot renew it.
  Missing or unreadable evidence is retried only when the route, typed cause,
  and retry limits would allow fallback once evidence is available. At expiry,
  missing proof refuses the route.
- Why: polling or process restart must not create more time or writer authority.

## Recovery preserves work and budget

A writer continuation retains predecessor work and the original review diff
base. Budgets survive resumption; terminal receipts remain immutable. Recovery
reopens selected consumers without restarting unrelated work.
Graph-wide retry with no work left to resume and no extension must select a
node or refuse with the node retry command. Pending work and budget extensions
remain resumable even when an advisory node failed. Prior model exclusions
use `adapter/id`; a carried graph exclusion that no longer resolves is dropped
with a warning. Explicit flag and standing configuration errors remain visible
to their author.
- Why: a retry must neither discard useful work nor erase the cost of failure.
