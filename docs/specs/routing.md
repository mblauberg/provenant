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
downgrade is inferred; a family comes only from provider protocol evidence at
the catalogue revision, never from a name or output text. A pool draw is seeded
from the operation, so a retry picks the same member without stored state, and
the receipt names what ran and why. Capability facts never grant filesystem or
network authority, and a confidential request admits only models that neither
train on nor retain prompts.
- Why: the receipt must name exactly what ran, and assurance claims must not be
  forgeable by output.
- Test: `crates/routing/src/tests/policies.rs::exact_unknown_is_retained_but_requires_availability`;
  `crates/routing/src/tests/selection.rs::table_driven_weighted_pools_record_effort_readiness_roll_and_seed`;
  `crates/routing/src/tests/alternatives.rs::family_requires_provider_answer_evidence_at_the_same_revision`;
  `crates/routing/src/tests/selection.rs::confidential_pool_excludes_training_models`

## One resolution, bounded alternatives

Resolution runs once against immutable snapshots and returns one route with at
most two ordered alternatives that pass the same checks; Run never rebuilds
candidates. A candidate excluded before handoff has not run, so preparation
moves to the next. `route list` and `route resolve` start no process and mutate
nothing.
- Why: the fallback plan is decided once and visible.
- Test: `crates/routing/src/tests/selection.rs::alternatives_are_bounded_distinct_and_keep_the_callers_order`;
  `crates/routing/src/fallback.rs::tests::preparation_switch_requires_local_custody_budget_and_unpinned_route`;
  `tests/route.rs::help_list_and_resolve_are_pure_and_resolve_reports_exact_candidates`

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

## Capability, account load and outcome preferences

Senior pools and review work prefer catalogue capability classes through positive
weights. `[domains.routing.capability_preferences.senior]` and `.review` override
the catalogue's `general`, `strong` and `frontier` weights; an empty selector
table disables that preference. The receipt records the class, effective weight
and catalogue or config source. Capability never removes a member: Terra and
below-preference models remain eligible, including when they are the only choice.

For each exact model, account selection ranks ready accounts by usable headroom
per second until reset, divided by one plus their live lane count. The limiting
applicable fresh quota window supplies headroom and reset time. Missing inputs
stay unknown in the receipt and use neutral ranking defaults. Authentication,
identity and measured exhaustion still refuse; reserve levels, unknown partial
headroom and the Go allowance floor apply positive ranking penalties with sourced
warnings. They also pass the pre-launch admission check. Account keys break ties,
so registration order cannot decide the chosen identity. The route and pool
receipts expose `account_load`, its availability source, input sources and warnings;
`ranking_factor_basis_points` records the combined pool factor separately from
its pacing factor.

Production dispatch reads host live lanes across registered projects and at most
the latest 1,000 retained attempts in the caller's project. Pool/role-local priors
count attributed successful completions, partial completions and quality failures;
missing attribution and infrastructure stops are not quality observations. A
Beta(1,1) prior shrinks low-sample estimates towards neutral. Receipts show counts,
role, pool and evidence source; no evidence stays neutral. An independent seeded
hash reserves a default 10% uniform exploration share among positive-weight ready
members. Configured model-choice overrides still take precedence.
`[domains.routing.outcome_priors] enabled = false` disables both outcome weighting
and exploration; `exploration_percent` configures the share from 0 through 100.
Store absence or unreadable evidence supplies unknown load and neutral priors,
never a routing gate. The established read-only SQLite API may update WAL/SHM
companions, but does not create a store or initialise its schema.
- Why (default): D8 (#3271) and features 5 and 6 of #3207 prefer stronger reviews,
  use capacity before reset and learn from outcomes without starving alternatives.
- Test: `crates/routing/src/tests/quality.rs::capability_priors_and_exploration_are_preferences_on_nonempty_draws`;
  `crates/routing/src/tests/quality.rs::account_ranking_uses_reset_adjusted_headroom_and_live_load_independent_of_order`;
  `crates/routing/src/tests/availability.rs::pacing_reserve_warns_without_excluding_accounts`;
  `crates/telemetry/src/tests.rs::route_priors_count_only_attributed_comparable_quality_outcomes`;
  `crates/cli/src/route/tests_2.rs::go_allowance_preference_never_refuses_a_ready_dispatch`

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
  `::full_refusal_file_accepts_probe_failure_and_restores_refusals`
- Owner: `crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`

## Login and access-token status

`refresh_pending` records a signed-in account whose access token needs renewal;
it stays eligible for routing so launch can ask the provider CLI to refresh.
`needs-login` / `AUTH_REQUIRED` requires a signed-out observation, rather than
access-token expiry. Access-token expiry and sign-in end are distinct facts.
Claude's sign-in end is `loginExpiresAt` from `auth status --json`, with a default
ten-day warning; it is informational and never alone vetoes a route. Reprobing
login uses each adapter's `login_reprobe` capability, including Claude's
`AccountIdentity`, so a new signed-in observation clears a stale login failure.

## Depletion handoff

For a graph stop, readiness observations older than one hour are unknown. A
pinned account remains pinned, and the graph does not take a higher tier as a
depletion fallback. A chosen tier travels with the graph route so the next
stop uses the same tier. If facts cannot be read, declared routes remain the
fallback. Pre-launch route switching requires local preparation and released
custody; launched stops follow the graph stop rule.

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

Providers never signal a process or publish completion. Structured errors
outrank claimed success, provider prose asserts nothing, and malformed frames
fail closed. A continuation binds the exact adapter, session, account, model and
controls. A launch spec is built in memory and persists only redacted.
Unsupported requested OpenCode effort is an actionable `unsupported_effort`
error naming the model and supported controls. A provider-side invalid model
or variant error names the requested model and variant in status and output,
with a remedy, without copying arbitrary provider messages or bodies.
- Why: a provider's text must not decide an outcome or switch identity, and the
  launch spec carries credentials.
- Test: `crates/providers/src/tests_stream_outcomes.rs::terminal_answer_and_nonzero_exit_remain_separate_facts`;
  `::continuation_preserves_session_and_rejects_changed_identity_and_grants`;
  `crates/providers/src/tests_accounts_and_limits.rs::selected_accounts_control_provider_environment_and_exact_authentication_grants`

## Accounts and credentials

Go and Zen each have one account identity across their model namespace. An
explicit Zen registration wins; otherwise the Go login directory supplies a single
Zen credential view, without inheriting Go quota, limits or pacing. Repeated model
entries never create additional account rows. Free-model pacing stays separate.
- Test: `crates/providers/src/accounts.rs::account_pool_tests::go_and_zen_share_one_account_row_per_identity_across_models`

An account identity is detected, never guessed; an undetectable one is
`unknown`, and a changed identity around a probe refuses. A pin filters
eligibility and never switches accounts. Credentials are never copied between
identity directories, logged, written, put on argv or passed in a child
environment. The sandbox never refreshes or writes a keychain; a lane that
holds a registered long-lived lane token is never rotated by the host.
- Why: misattribution spends or exposes the wrong account, and one login renewal
  must not end every live lane on it.
- Test: `crates/providers/src/quota_live/claude.rs::fixed_directory_rejects_changed_account_fingerprint`;
  `crates/routing/src/tests/availability.rs::account_override_selects_a_pool_member_and_keeps_other_provider_fallbacks`;
  `crates/providers/src/tests_account_identity.rs::claude_lane_prefers_the_registered_lane_token_and_never_rotates_the_login`.
  No test asserts tokens are absent from argv and child environments (gap).
- Owner: `crates/providers/src/identity.rs`, `crates/providers/src/claude.rs`

## Launch route and role tiers

`run --route ROUTE` names one alias for the whole graph, or a role map such as
`--route writer=junior,review=senior` that sets a tier per node role. The
roles are `writer`, `review`, `synthesis`, `task` and `plan`; a node takes
`map[role]` or keeps its template default. An unknown tier or role is a usage
error that lists the valid values. `run` has no `--tier` (`land --tier` is
unchanged). Template defaults are data: `implement` runs as junior only for a
bounded brief, a plan-node output or a `batch` slice. An unplanned
implementation gets a senior sizing plan before its junior writer; other
templates keep their tiers. Tier changes at run time belong to the retry and
depletion rules, not to launch.
- Why: a caller should choose cost per role without editing a template, and a
  typo must name the valid choices instead of launching on the wrong tier.
- Test: `src/fabric/interface.rs::a_role_map_sets_each_node_to_its_role_tier_or_keeps_the_default`;
  `src/fabric/interface.rs::an_unknown_route_role_is_refused_listing_the_valid_roles`;
  `crates/fabric-runtime/src/graph/implement_invariants.rs::implementation_is_planned_unless_its_brief_is_already_bounded`
- Owner: `src/fabric/interface.rs`, `crates/fabric-runtime/assets/templates/`

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
  `::successful_parent_closes_attached_children_but_not_detached_children`
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `crates/fabric-runtime/src/graph/relay.rs`

### Host-pinned Git

Git is resolved and validated on the host and pinned as the only Git a lane may
run; a replaced or loosened target is refused. Writers use a private Git
directory with the shared object store read-only, and the host imports and
publishes the result itself.
- Why: a lane-chosen Git or helper would run with host authority.
- Test: `crates/os/src/git.rs::override_wins_and_invalid_override_does_not_fall_back`;
  `crates/confinement/src/utility.rs::executable_pin_keeps_the_locator_and_follows_a_swapped_target`
- Owner: `crates/os/src/git.rs`
