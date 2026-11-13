# Retry owner

Owner: `crates/fabric-runtime` (graph recovery), `crates/run` (attempt custody),
`crates/routing` (allowance and exclusions), `crates/confinement` (writer census)
Anchor: `crates/routing/src/retry.rs`,
`crates/fabric-runtime/src/graph/graph_composite_and_allowance_tests.rs`,
`crates/fabric-runtime/src/graph/graph_launch_retry_tests.rs`

## One allowance per lineage

Recovery preserves terminal receipts and predecessor links. Node, template and
host allowances resolve once with their origin; graph and run must not multiply
or renew that allowance. Observation replay and proven pre-launch preparation
fallbacks spend no settlement retries. Scoped account refusals remain scoped.
`provenant config schema` owns defaults.
- Why: retries must remain bounded and explainable across restart.
- Test: `crates/routing/src/retry.rs::tests::allowance_precedence_and_origin`;
  `crates/fabric-runtime/src/graph/graph_composite_and_allowance_tests.rs::cumulative_allowance_counts_graph_attempts_and_survives_observation_replay`;
  `crates/fabric-runtime/src/graph/graph_composite_and_allowance_tests.rs::preparation_fallback_does_not_consume_settlement_allowance`

## Admission is distinct from observation

A settlement wait observes work; it does not renew admission. Each admission
window persists one deadline and next wake under the same operation identity.
Transient, typed contention may back off within that window. An uncertain
handoff reobserves the same guardian and route rather than launching a second
provider. Unknown writer ownership remains fail-closed.
- Why: polling or process restart must not create more time or writer authority.
- Test: `crates/fabric-runtime/src/graph/graph_launch_retry_tests.rs::launch_errors_retry_until_the_persisted_deadline`;
  `crates/fabric-runtime/src/graph/graph_launch_retry_tests.rs::uncertain_handoff_reobserves_one_guardian_on_the_same_route`

## Recovery preserves work and budget

A writer continuation retains predecessor work and the original review diff
base. Budgets survive resumption; terminal receipts remain immutable. Recovery
reopens selected consumers without restarting unrelated work.
- Why: a retry must neither discard useful work nor erase the cost of failure.
- Test: `crates/fabric-runtime/src/graph/graph_composite_and_allowance_tests.rs::writer_quota_continuation_succeeds_and_lens_diff_base_stays_node_source`;
  `crates/fabric-runtime/src/graph/retry_tests.rs::node_redo_retains_spent_budget_and_unknown_usage_across_attempt_generations`;
  `crates/fabric-runtime/src/graph/retry_tests.rs::retry_node_and_dependants_resets_consumers_and_preserves_unrelated_work`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::continuation_budget_pauses_then_extends_without_duplicating_prior_work`

Test: none (gap) for atomic terminal publication across all owners, and for
every real-provider custody failure. Those requirements
remain review gates; unit recovery evidence does not prove native enforcement.

A launched stop follows the graph stop rule. Preparation fallback only switches
with evidence that custody was released before launch.
