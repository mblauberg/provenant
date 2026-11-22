# Retry owner

Owner: `crates/fabric-runtime` (graph recovery), `crates/run` (attempt custody),
`crates/routing` (allowance and exclusions), `crates/confinement` (writer census)
Anchor: `crates/routing/src/retry.rs::RetryAllowance`,
`crates/fabric-runtime/src/graph.rs`, `crates/run/src/lib.rs`

## One allowance per lineage

Recovery preserves terminal receipts and predecessor links. Node, template and host allowances
resolve once with their origin; graph and run never multiply or renew that allowance.
Observation replay and proven pre-launch preparation fallbacks spend no settlement retries, and
scoped account refusals stay scoped. `provenant config schema` owns the defaults.
- Why: retries must stay bounded and explainable across restart.

## Admission is distinct from observation

A settlement wait observes work; it does not renew admission. Each admission window persists one
deadline and next wake under the same operation identity, and transient typed contention may back
off within that window. An uncertain handoff reobserves the same guardian and route rather than
launching a second provider, and unknown writer ownership fails closed. Preparation fallback uses
the host's typed failure and local custody disposition: a local handoff failure records its
preparation cause after withdrawing the unlaunched lease, and an attempt proven never to have
launched needs no provider custody lock, though an existing lease or writer reservation must
still be released. Boundary refusals and uncertain withdrawals authorise no other launch.
- If a terminal row is visible before local custody release, a route fallback re-reads the same
  attempt's evidence after a short persisted wake. The wake is bound to that attempt, expires
  after one second and cannot be renewed by replay. Missing or unreadable evidence is retried
  only when the route, typed cause and retry limits would allow fallback once evidence is
  available; at expiry, missing proof refuses the route.
- Why: polling or process restart must not create more time or writer authority.

## Recovery preserves work and budget

A writer continuation keeps predecessor work and the original review diff base. Budgets survive
resumption and terminal receipts stay immutable. Recovery reopens selected consumers without
restarting unrelated work. A graph-wide retry with no work left to resume and no extension must
select a node or refuse with the node retry command. Pending work and budget extensions stay
resumable even when an advisory node failed.

Lowered host structural limits pause an unchanged graph on resume or graph-wide retry, saving the
structural-limit pause; only a changed candidate refuses admission. The pause admits no new launch
but never defers observing an attempt the host already accepted: a terminal attempt still settles
its node and publishes the writer's final commit from the bound branch, so a wait keyed on node
state converges. A saved attempt with no host reference is not started under the pause. The pause
persists while any node is live and the graph is over the lowered limit, and clears when the graph
fully settles or the limits are raised.

Prior model exclusions use `adapter/id`; a carried graph exclusion that no longer resolves is
dropped with a warning, while explicit flag and standing configuration errors stay visible to
their author.
- Why: a retry must neither discard useful work nor erase the cost of failure, and returning from a
  resume before observing a terminal attempt would wedge its node in `running` with a base head.

## Credential admission and publication

Operator brief admission checks held exact credential values only when `redaction` is `secrets`
([supervisor](supervisor.md#disclosure-and-safe-git-evidence) owns the setting); under `off` it
checks nothing. A match warns and proceeds, and the path never opens credential stores or refuses
an operator brief. It skips ambient values beyond its bounds (`crates/redact/src/lib.rs`) with
one warning per process naming neither names nor values.

When egress masking is enabled, publication collects ambient values strictly: an over-bound
credential set fails validation and withdraws the affected publication, including text, streams,
diagnostics and launch publication context.
