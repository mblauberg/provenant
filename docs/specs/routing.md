# Routing, providers and quota

Owner: `crates/routing` (catalogue, resolve), `crates/providers` (adapters,
accounts, decoding), `crates/quota` (windows, refusals, readiness)
Anchor: `crates/routing/src/tests.rs`, `crates/providers/src/tests.rs`,
`crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`

How a dispatch becomes one exact provider model, and how quota and readiness
gate it. Policy and defaults are overridable data
(`provenant config schema`). Paths are relative to
`tools/front-door/`.

## Exact selection, no inference

A request names one exact model or route. No adapter, upgrade, effort or
downgrade is inferred. An adapter may declare that a valid but unsupported
catalogued model effort is dropped before admission; the applied route and
launch record retain its warning and source. Invalid controls remain errors.
A family comes only from provider protocol evidence at
the catalogue revision, never from a name or output text. A pool draw is seeded
from the operation, so a retry picks the same member without stored state, and
the receipt names what ran and why. Capability facts never grant filesystem or
network authority, and a confidential request admits only models with an
explicit catalogue guarantee that they do not train on prompts. Unknown
training treatment is excluded.
- Why: the receipt must name exactly what ran, and assurance claims must not be
  forgeable by output.
- Test: `crates/routing/src/tests/policies.rs::exact_unknown_is_retained_but_requires_availability`;
  `crates/routing/src/tests/selection.rs::table_driven_weighted_pools_record_effort_readiness_roll_and_seed`;
  `crates/routing/src/tests/alternatives.rs::family_requires_provider_answer_evidence_at_the_same_revision`;
  `crates/routing/src/tests/selection.rs::confidential_pool_excludes_training_models`;
  `crates/routing/src/tests/policies.rs::opencode_effort_normalisation_is_visible_in_exact_pool_and_launch_routes`

## Repository confidentiality at launch

The host classifies declared inputs and observed changes before route selection.
Protected inputs require no-training treatment for every candidate, including
fallback; incomplete evidence cannot silently declassify them. Recovery retains
the accepted classification. This does not classify arbitrary later file access.

- Why: confidential inputs must not reach a model that may train on them.
- Test: `crates/cli/src/run/launch_policy.rs::tests::repository_classification_covers_inputs_briefs_diff_and_recovery`; `crates/routing/src/tests/selection.rs::confidential_pool_excludes_training_models`

## One resolution, bounded alternatives

Resolution selects a route and bounded, ordered eligible alternatives from one
snapshot. Preparation fallback follows [retry ownership](retry-owner.md).

- Why: the fallback plan is decided once and visible.
- Test: `crates/routing/src/tests/selection.rs::alternatives_are_bounded_distinct_and_keep_the_callers_order`

## Policy is overridable data

Bundled model policy, pools and writer advice are data that user, scope and
account rules overlay. A malformed restrictive rule refuses configuration and
never broadens; an optional preference warns and grants nothing. A model-choice
preference always expires.
- Why: changing operating policy should not need a Rust change, and a typo must
  not widen access.
- Test: `crates/routing/src/policy.rs::tests::schedule_validation_refuses_malformed_bounds_windows_and_zones`;
  `crates/routing/src/policy.rs::bundled_fields_and_peak_schedule_can_be_overridden_per_account`;
  `crates/routing/src/tests/preferences.rs::model_choice_selects_only_eligible_pool_members_before_expiry`

## Capability and selection preferences

Capability never excludes a listed pool member. Capability preferences rank
members alongside account load and outcome priors; configuration can adjust
those weights. Exact model pins remain explicit choices.

Account load and outcome priors rank eligible members. Priors use attributed,
comparable quality outcomes, with exploration among eligible members;
host failures are not quality observations, while a validated successful
completed-before-budget result remains one. Missing outcome evidence stays
neutral. Account ranking scales reset-adjusted usable headroom by sourced
penalties and divides by one plus live lanes.

- Why: capability is a preference, while attributed comparable outcomes supply the evidence that adjusts declared route weights.
- Test: `crates/routing/src/tests/quality.rs::outcome_priors_and_exploration_rank_eligible_members`; `crates/routing/src/tests/quality.rs::account_ranking_uses_reset_adjusted_headroom_and_live_load_independent_of_order`; `crates/telemetry/src/tests.rs::route_priors_count_only_attributed_comparable_quality_outcomes`

## Capacity is not quota

Capacity (`model_unavailable`) and account allowance (`quota_limited`) are
separate outcomes. A capacity cooldown is an advisory selection preference that
explicit models ignore; only a quota-limited outcome cools an account. Retries
spend the single lineage allowance in [retry owner](retry-owner.md).
- Why: capacity and quota need different remedies without multiplying retries.
- Test: `crates/providers/src/tests_stream_outcomes.rs::agy_overload_is_typed_capacity_not_a_generic_failure`;
  `tests/graph_provider.rs::codex_capacity_settles_without_quota_refusal`;
  `crates/quota/src/schedule.rs::tests::report_shows_only_live_capacity_cooldowns`

## Unknown never means ready

Unknown or stale quota never causes a refusal, and missing, expired or
mismatched readiness evidence never reads as ready. Refusals are scoped to
model and account unless provider evidence proves a wider pool; a plan usage
limit holds the whole account until its reset. One function,
`Admission::account_block`, decides account blocks for both resolution and
the pre-launch recheck. Readiness is stored apart from quota records; a failed
publication leaves readiness unknown and never erases held refusals.
- Why: one probe outage or model limit must not take a provider offline, and
  no stale evidence may admit a launch.
- Test: `crates/quota/src/admission.rs::unknown_and_stale_do_not_manufacture_exhaustion`;
  `crates/quota/src/admission.rs::admission_tests::account_usage_limit_holds_every_model_until_its_reset`;
  `crates/quota/src/admission.rs::independent_pool_reset_and_refusal_scope`;
  `crates/routing/src/tests/exclusions.rs::persisted_mismatch_excludes_account_and_selects_another`;
  `crates/quota/src/readiness.rs::tests::readiness_ready_evidence_requires_fresh_matching_scope`;
  `crates/quota/src/readiness.rs::tests::full_refusal_file_accepts_probe_failure_and_restores_refusals`
- Owner: `crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`



## Revisioned, bounded refresh

A quota refresh commits only against the revision it claimed, so stale
completions are discarded. Ordinary probes keep their tick deadline. A CLI
credential rotation has a separate admission budget and, once started, runs to
completion so its single-use refresh token can be persisted safely. A short
budget returns a typed deferred result without starting the refresh.
- Why: the menu bar ticks unattended and concurrently.
- Test: `crates/quota/src/schedule.rs::competing_claims_and_stale_completions_cannot_overwrite`;
  `tests/quota.rs::configuration_cannot_raise_probe_limits_and_publication_failure_is_io`

## Providers decide nothing

Providers do not publish completion or decide outcomes from prose. Structured
errors outrank claimed success, malformed frames fail closed, and continuation
preserves its selected identity and controls.

- Why: provider text must not decide an outcome or switch identity.
- Test: `crates/providers/src/tests_stream_outcomes.rs::terminal_answer_and_nonzero_exit_remain_separate_facts`; `crates/providers/src/tests_stream_outcomes.rs::continuation_preserves_session_and_rejects_changed_identity_and_grants`

## Accounts and credentials

Account identity is detected, not guessed; a changed identity around a probe is
refused. A pin does not switch accounts. Credential custody is owned by
[supervision](supervisor.md#native-environment-and-sandbox-tool-stores). A registered long-lived lane token is not rotated by the host.

OpenCode models are partitioned by the registered plan, independently of account
keys and labels. Enumeration keeps one row per registered identity and never
invents a Zen identity from Go credentials. Plan declarations also own the quota
source: missing or unrecognised plans retain unknown quota evidence rather than
using the Go ledger. Free-model pacing and refusals remain
scoped to the declared free pool.

- Why: misattribution can spend or expose the wrong account.
- Test: `crates/providers/src/accounts.rs::account_pool_tests::registered_plans_enumerate_once_without_inventing_identities`; `crates/routing/src/tests/availability.rs::registered_opencode_plans_select_independently_of_account_keys`; `crates/providers/src/quota_live/claude.rs::current_registration_requires_its_matching_account_fingerprint`; `crates/routing/src/tests/availability.rs::account_override_selects_a_pool_member_and_keeps_other_provider_fallbacks`; `crates/providers/src/tests_account_identity.rs::claude_lane_prefers_the_registered_lane_token_and_never_rotates_the_login`

## Launch route and role tiers

A role-specific route applies to matching graph nodes; other nodes retain their
template selection. Template defaults are data.

- Why: callers can choose a route by role without editing a template.
- Test: `src/fabric/interface.rs::a_role_map_sets_each_node_to_its_role_tier_or_keeps_the_default`

## Nested lanes

A lane's environment pins its depth, parent attempt and state directory. Inside
a lane Fabric refuses coordination mutations; graph submission and reads cross
the attempt-bound host relay.
- Why: a lane must not move claims or leases.
- Test: `crates/providers/src/lane.rs::pinned_environments_have_attempt_unique_agent_labels`

### The host validates and launches children

A child launch is journalled and launched by the host, bound to the calling
attempt rather than a lane-written record. Children share the graph's budget
and cancellation and may use another adapter and account within the user's
authority; ordinary launch policy still refuses credential and state roots.
Parent settlement closes attached children, never detached ones.
- Why: cross-family delegation must not make lane-controlled identity
  authoritative.
- Test: `src/fabric/interface.rs::tests::child_launch_crosses_the_lane_transport_and_provisions_writer_fanout`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::writer_children_get_their_own_worktree_and_depth_is_bounded`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::successful_parent_closes_attached_children_but_not_detached_children`
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `crates/fabric-runtime/src/graph/relay.rs`

### Host-pinned Git

The host resolves and validates the Git executable pinned for lane use; a
replaced or loosened target is refused.

- Why: lane-chosen executables must not run with host authority.
- Test: `crates/os/src/git.rs::override_wins_and_invalid_override_does_not_fall_back`; `crates/confinement/src/utility.rs::executable_pin_keeps_the_locator_and_follows_a_swapped_target`
