# Routing, providers and quota

Owner: `crates/routing` (catalogue, resolve), `crates/providers` (adapters,
accounts, decoding), `crates/quota` (windows, refusals, scheduling)
Anchor: `crates/routing/src/tests.rs`, `crates/providers/src/tests.rs`,
`crates/quota/src/admission.rs`

This spec owns how a dispatch becomes one exact provider model, how adapters
build and decode launches, and how quota decides readiness. Run owns
admission and publication ([run](run.md)); supervisor and confinement own
custody and grants ([supervisor](supervisor.md)). Grammar is in the
generated [CLI reference](../reference/cli-all.md), keys in the [config
reference](../reference/config-keys.md), and bundled pools in
`provenant route list`. Paths are relative to `tools/front-door/`.

## Resolution

### Bundled policy data and overrides

`crates/routing/assets/model-policy.toml` supplies embedded `ModelPolicyRule`
data, including cache ratios, compaction windows, lane and result guidance,
and DeepSeek's peak adjustment. User defaults overlay the bundled result,
then scoped rules overlay those defaults. Account rules can override any
bundled field. The price table keeps base rates without a second schedule.
Writer advice uses exact catalogue classes and pool order in
`crates/routing/assets/writer-advisory.toml`, replaceable under `domains.routing.writer_advisory`.
Provider-family assurance remains established by provider evidence.
- Why (default): changing operating policy should not require a Rust branch or model-name inference.
- Test: `crates/routing/src/policy.rs::tests::bundled_policy_has_valid_rules_and_schedules`;
  `crates/routing/src/policy.rs::bundled_fields_and_peak_schedule_can_be_overridden_per_account`;
  `crates/routing/src/catalog.rs::tests::writer_advisory_classes_and_pool_priority_are_overridable`
- Owner: `crates/routing/src/{policy,catalog}.rs`

### Exact selection, seeded pools

A dispatch carries its requested write and effective network controls into
selection. Each candidate must support those controls in its adapter's launch
table, and preparation checks them again before launching. Unsupported explicit
requests are refused; unsupported pool members are excluded. Capability facts
never grant filesystem or network authority.

A request names exactly one `--model ADAPTER/ID` or `--route NAME`. No
adapter, upgrade, effort default or downgrade is inferred, and an unknown
explicit ID runs only if availability, disclosure and confinement are
independently established. A pool draws among ready members, weighted by
the provider's pacing factor (0.25 to 2.0), optional cache preference and a
seed from the operation ID and task index. The receipt records each weight and
its config or catalogue source, pacing policy and source, exclusions, readiness,
the draw rule, random roll and chosen member. An attempt records the resolved
model, effort and account label, with `source` identifying a pool, named route,
explicit pin, fallback or continuation. The receipt lists considered exclusion
rules even when another account keeps a member eligible; the launch record names
whether account selection was pinned, automatic or continued. For a fallback,
`pool_selection.pick` retains the original pool draw; `source = fallback`
identifies the route actually launched. The receipt contains routing evidence,
not prompt or credential text.
- Why (default): retries pick the same model without stored state, and the receipt
  names what ran.
- Test: `crates/routing/src/tests/policies.rs::exact_unknown_is_retained_but_requires_availability`;
  `crates/routing/src/tests/selection.rs::table_driven_weighted_pools_record_effort_readiness_roll_and_seed`
- Owner: `crates/routing/src/`

### Capacity cooldown

A host records a short-lived capacity cooldown for the affected model and account
when a provider reports capacity. Pool-backed routes and senior/junior draws skip
live cooldowns; if all candidates are cooling, the normal choice remains available
and the receipt explains the fallback. Explicit model dispatches and resumes ignore
capacity cooldowns. Quota refusals remain separate and continue to apply. The host
setting `domains.routing.capacity_cooldown_seconds` defaults to 600 seconds; zero disables new cooldowns.
`quota` and `status` show live cooldowns and their remaining time.
- Test: `crates/quota/src/schedule.rs::tests::reserve_adopts_a_renamed_label_for_the_same_account_key`;
  `crates/quota/src/schedule.rs::tests::report_shows_only_live_capacity_cooldowns`;
  `crates/cli/src/run/tests_lanes.rs::lanes_show_only_live_capacity_cooldowns`
- Test gap: no current end-to-end test pins cooldown persistence and explicit-model bypass.

### Settlement retry

Adapters distinguish capacity (`model_unavailable`) from account allowance
(`quota_limited`). A transient 429 is not proof of exhausted quota. Recovery
must preserve the resolved authority, release custody before a successor and
spend the single lineage allowance in [retry owner](retry-owner.md). That spec
owns budgets, admission deadlines and continuation identity.
- Why (default): capacity and quota need different remedies without multiplying retries.
- Test: `crates/providers/src/tests_stream_outcomes.rs::agy_overload_is_typed_capacity_not_a_generic_failure`;
  `crates/providers/src/tests_stream_outcomes.rs::codex_usage_limit_is_quota_limited`;
  `crates/providers/src/claude.rs::overload_tests::usage_limit_is_a_typed_quota_refusal_not_a_generic_failure`;
  `tests/graph_provider.rs::codex_capacity_settles_without_quota_refusal`;
  `tests/graph_provider.rs::named_route_writer_retries_quota_with_partial_work`
- Owner: `crates/fabric-runtime/src/graph/`, `crates/routing/src/retry.rs`

### One resolution, bounded alternatives

Resolution runs once against immutable catalogue, availability and quota
snapshots and returns one route with at most two ordered alternatives, each
passing the same checks; Run never rebuilds candidates. A candidate that
local provider preparation excludes before handoff has not run, so preparation
advances past it, whatever the exclusion, to the next alternative or, for a
named pool, the next untried permitted member; each excluded candidate is
listed in `skipped_routes` with its typed kind and cause. Pins still apply, and
`settlement_retries` bounds launched attempts, not these candidates. A missing
access context is `confinement_unavailable`; only an explicit disclosure refusal
is `disclosure_denied`. Separately, a configured read-only result-quality check may
escalate an invalid successful result after custody release; task failures and
uncertain outcomes cannot trigger that escalation. Unsupported effort fails; where clamping is permitted, the requested,
applied and observed values are recorded. `route list` and `resolve` start no
process and mutate nothing.
`route resolve` displays fresh account quota snapshots for the requested model,
alongside scoped readiness failures. A quota-derived ready observation identifies
its source and is not an executable or login probe. Its admission preview checks
the eligible accounts' quota using the launch admission rules; an unprobed
provider alone is not an admission refusal. A launch still probes availability
and rechecks current evidence before starting.
- Why (default): the fallback plan is decided once and visible.
- Test: `crates/routing/src/tests/selection.rs::alternatives_are_bounded_distinct_and_keep_the_callers_order`;
  `crates/routing/src/fallback.rs::tests::switching_requires_safe_outcome_and_every_guard`;
  `crates/routing/src/tests/availability.rs::availability_access_and_scoped_quota_are_independent_gates`;
  `crates/cli/src/run/provider/tests_writer_scratch.rs::provider_fence_preparation_reports_causes_and_selects_available_candidate`;
  `crates/cli/src/route/tests_1.rs::resolve_after_quota_tick_uses_the_published_snapshot`;
  `tests/route.rs::help_list_and_resolve_are_pure_and_resolve_reports_exact_candidates`;
  `tests/route.rs::resolve_uses_fresh_account_quota_and_reports_only_real_quota_refusals`
- Owner: `crates/routing/src/`

### Model policy

Model/account policy, UTC schedules and expiring overrides are documented in
[model policy](../runbooks/model-policy.md). Dispatch supplies actual write
intent, applies guidance and the Claude window, and checks restricted results
before success. Run retains bounded host-owned retries as separate attempts in
the same run. Telemetry reporting reads existing categories without changing
capture. Policy and pacing share the validated `Schedule` in
`crates/routing/src/policy.rs`; schedules must not broaden a restriction when
malformed. Invalid restrictive rules refuse configuration; optional preference
errors warn without becoming authority. The generated config and CLI references
own schedule fields and override syntax.
- Test: `crates/routing/src/policy.rs::tests::schedule_validation_refuses_malformed_bounds_windows_and_zones`;
  `crates/routing/src/policy.rs::tests::shared_schedule_evaluator_handles_weekday_and_local_windows`
- Owner: `crates/routing/src/policy.rs`

### Expiring model choice preferences

Model choice preferences expire and require both risk-tier and task-kind
context. The last matching preference wins until expiry; malformed or missing
expiry cannot create an indefinite preference. The [CLI reference](../reference/cli-all.md)
owns override syntax and the [config reference](../reference/config-keys.md)
owns fields.

A dispatch with both context values prefers the model on its named route, even
one outside the route's pool: the operator accepted it explicitly and it expires,
whereas limiting it to the pool would make a suggestion for another model do
nothing. The normal account, confidentiality, exclusion, launch and model-policy
checks still apply; unavailable candidates fall back to the ordinary route. An explicit model pin wins. Dispatch copies `--risk-tier`
and `--task-kind` into task context; template submission copies them into graph
context. Graph authors can set the same fields. Missing context does not match.

- Why (default): a reviewed outcome can guide later route selection without extending
  authority or making an indefinite policy change.
- Test: `crates/routing/src/tests/preferences.rs::model_choice_selects_only_eligible_pool_members_before_expiry`,
  `crates/routing/src/tests/preferences.rs::model_choice_outside_route_obeys_restrictions`,
  `crates/cli/src/route/tests_2.rs::model_override_set_and_clear_preserve_expiry_and_pacing`
- Owner: `crates/routing/src/resolve.rs#select_pool`,
  `crates/cli/src/route.rs#set_model_choice`

### Go monthly allowance floor

Every OpenCode Go candidate, including an explicit or uncatalogued exact ID,
must have a recorded monthly allowance at least equal to
`domains.routing.opencode_go.min_monthly_allowance_usd`. The default is $60;
zero disables the check. Unknown allowances are refused when the floor is set.
Refusals identify the model, allowance or unknown status, and the setting.
Zen is a separate account and is unaffected. The floor is independent of
quota calibration and pacing.
- Why (default): protect the shared Go plan from low-allowance models regardless of
  selection path.
- Test: `crates/routing/src/catalog.rs::go_allowance_tests::go_allowance_floor_handles_limits_unknown_and_zen`,
  `crates/cli/src/route/tests_2.rs::go_floor_gates_explicit_pool_and_fallback_dispatch`
- Owner: `crates/routing/src/`

### Go pool quota coverage

OpenCode Go local ledger usage is labelled `source: local-ledger` and
`coverage: partial`; it is a lower bound because other devices are absent.
An active per-account calibration supplies dashboard percentage floors,
applied as the maximum of ledger and floor at each read. Expiry restores raw
ledger usage. Missing or expired floors make the affected partial headroom
unknown and ineligible by default. The `routing.allow_unknown_partial_headroom`
knob opts into selection and admission with that uncertainty; an observed lower
bound at the limit still excludes the member. Route explanation reports the
setting and its source. Exhaustion, authentication and quota limits still exclude members.
Configuration and the operational limits are
in the [calibration runbook](../runbooks/configuration.md#opencode-go-quota-calibration).
- Why (default): selection and launch admission use the same scoped quota evidence.
- Test: `crates/quota/src/calibration.rs::tests::calibration_takes_maximum_and_expires_even_with_cached_ledger`
- Owner: `crates/quota/src/calibration.rs`, `crates/routing/src/resolve.rs`

### Confidentiality

A confidential request admits only models whose catalogue row says they do
neither train on nor retain prompts; price does not affect eligibility. Fast or priority service is never enabled.
- Why (default): prompts carry private code; cost is not a privacy signal.
- Test: `crates/routing/src/tests/selection.rs::confidential_pool_excludes_training_models`
- Owner: `crates/routing/src/`

### Required provider families on panels and lenses

Review family constraints are optional hard eligibility requirements. A panel's
`minimum_family_counts` counts lenses, and a lens's `required_family` restricts
that lens's eligible routes. A lens requirement also contributes to the panel
minimum. Shape and request minimums combine by taking the larger count per
family, so a request cannot weaken a configured floor.

Panel expansion carries these declarations to route selection. Selection uses
`AdapterSpec.family` as its single source of provider identity and applies the
same candidate eligibility rules before allocating constrained legs. Assignment
must consider the whole panel: taking a flexible lens for one family must not
prevent a feasible assignment for another. A failed assignment refuses before
launch and reports the family, required count and candidate exclusion reasons;
`--explain` uses this same preflight. Resolved slots retain `satisfied_family`
and constrained routes retain the family filter through dispatch and fallback.
Unconstrained panels perform no extra draws and retain their existing expansion.

Adapter family is a routing declaration, separate from the verified protocol
evidence required for family assurance below.
- Why (default): a panel can meet a family floor without pinning all its lenses to one model.
- Test: `crates/routing/src/review.rs::tests::family_floors_keep_shape_minimum_and_record_matching_legs`;
  `crates/routing/src/review.rs::tests::required_lens_family_counts_towards_panel_minimum_and_refuses_with_reason`;
  `crates/routing/src/review.rs::tests::unsatisfied_panel_family_reports_count_and_candidate_exclusions`;
  `crates/cli/src/fabric/graph_relay.rs::panel_tests::review_family_constraints_use_eligible_pools_and_survive_template_expansion`
- Owner: `crates/routing/src/review.rs#PanelResolution`,
  `crates/routing/src/resolve.rs#resolve_for_account_with_seed_excluding_family`,
  `crates/records/src/adapters.rs#AdapterSpec`

### Families come from evidence

A model family is established only by provider protocol evidence verified
against the catalogue revision, never inferred from spelling, a broker name or the
requested ID. Decoders read documented event positions and never search
tool or subagent output for the answering identity.
- Why (default): cross-family review is an assurance claim and must not be forgeable
  by output text.
- Test: `crates/routing/src/tests/alternatives.rs::family_requires_provider_answer_evidence_at_the_same_revision`;
  `crates/providers/src/tests_adapter_results.rs::synthetic_and_api_error_messages_cannot_change_answering_identity`
- Owner: `crates/providers/src/`

### Closed adapter table

`records::ADAPTER_SPECS` is the closed adapter table that config, CLI and
`providers::REGISTRY` share. Dispatch probes only the adapters that its route, pool
and fallbacks name. Tier preference is guidance owned by the orchestrate
skill; it never waives authority, confinement or assurance gates.
- Why (default): one closed list keeps every surface agreeing; routing grants no
  authority.
- Test: `crates/cli/src/install.rs::adapter_specs_cover_config_routing_and_cli_names`;
  `crates/routing/src/tests/availability.rs::probe_adapters_include_only_requested_routes_pools_and_fallbacks`
- Owner: `crates/records/src/adapters.rs`

## Providers

### Answer and exit are separate facts

Providers never signal a process or publish completion. Structured errors
outrank claimed success, provider prose about quota asserts nothing, and
malformed or oversized frames fail closed. Session continuation binds the exact
adapter, session, account, host, model and controls.
- Why (default): a provider's text must not decide an outcome or switch identity.
- Test: `crates/providers/src/tests_stream_outcomes.rs::terminal_answer_and_nonzero_exit_remain_separate_facts`;
  `crates/providers/src/tests_stream_outcomes.rs::continuation_preserves_session_and_rejects_changed_identity_and_grants`
- Owner: `crates/providers/src/`

### Launch specs and adapter isolation

`providers::prepare` builds the exact argv, environment and profile in
memory; only a redacted summary persists, and there is no raw caller argv.
Each adapter is subordinate to the outer profile: Codex's own sandbox is
off; Claude gets its token on a descriptor closed before spawn, never in the
environment; Agy's private home links only one keychain, the selected account's own
(`Library/Keychains/login.keychain-db` in its registered directory, refused when
missing) or, for the user's own home, the login keychain; Cursor
executes only its resolved version; OpenCode’s read-only lanes deny file edits and web fetch
but permit shell commands, as Claude, Codex and Agy readers do, so read-only
lenses can atomically write progress in their own private state. The rendered
profile, not OpenCode’s permission config, refuses writes to the source and to
protected checkouts.
Agy’s selected native keychain mechanism permits its client and IPC in both
account modes; see [authentication diagnosis](../runbooks/agy-authentication.md).
Exact argv is pinned by `tests/fixtures/providers/<adapter>/recordings.json`.
- Why (default): the launch spec carries credentials and authority.
- Test: `crates/providers/src/tests_accounts_and_limits.rs::selected_accounts_control_provider_environment_and_exact_authentication_grants`;
  `crates/providers/src/tests_lane_toolchains.rs::agy_runs_in_a_private_home_that_links_only_the_login_keychain`;
  `crates/providers/src/tests_lane_toolchains.rs::agy_isolated_account_links_its_own_keychain_and_grants_only_that_path`
- Owner: `crates/providers/src/`

### Agy read-only commands and denials

Agy auto-approves tools in headless read-only and writer lanes. The outer
confinement profile is the boundary: read-only lanes may execute system
programs from `/bin`, `/usr/bin` and `/usr/libexec`, with PATH restricted to
`/usr/bin:/bin` and global and system Git configuration disabled. Their only
write grants remain attempt scratch and private state. Like read-only Codex
lanes, read-only Agy lanes can run system programs with the lane's existing
network access, so read roots must not hold anything the lane may not see.
A denied empty result fails the attempt. Terminal diagnostics must not quote
provider-controlled denial contents; retained captures preserve the evidence.
- Why (default): headless permission prompts otherwise deny read-only commands, and a
  remaining denial must not be mistaken for an empty response.
- Test: `crates/providers/src/tests_launch_access.rs::spilled_prompts_reach_argv_adapters_with_file_read_permissions`;
  `crates/providers/src/tests_launch_access.rs::agy_writers_exec_what_codex_writers_exec`;
  `crates/providers/src/seatbelt_snapshots.rs::agy_read_only_compiled_profile_denies_read_root_write`;
  `tests/graph_provider.rs::agy_denial_keeps_raw_step_out_of_terminal_diagnostics`
- Owner: `crates/providers/src/agy.rs`, `crates/cli/src/run.rs`

### Claude token refresh

The sandbox never refreshes or writes the keychain. The host refreshes a
token with under 15 minutes left, using one unconfined CLI run with every
customisation off, in a fresh private directory removed afterwards, and
never copies credentials between identities. Host launches and quota refreshes
serialise the credential read, refresh and reread under one exclusive account
config-directory lock, bounded by the caller's deadline and a 60-second cap.
Shell-capable Claude lanes receive only the access token through a pipe,
use an empty private config directory, scrub child environments and deny
keychain reads. Claude still holds the
token and has outbound HTTPS, so isolation is incomplete.
- Why (default): credential writes belong to the user's own sign-in flow.
- Test: `crates/providers/src/tests_account_identity.rs::claude_login_refreshes_only_near_expiry_with_customisations_off_and_never_prints_the_token`;
  `crates/providers/src/tests_account_identity.rs::claude_concurrent_host_refreshes_do_not_reuse_a_rotating_token`;
  `crates/providers/src/tests_accounts_and_limits.rs::claude_writer_shell_has_private_config_and_rust_tools`
- Owner: `crates/providers/src/claude.rs`, `crates/providers/src/host.rs`

## Accounts

### Identity is detected, never guessed

Labels are non-secret and never contain emails, token fragments or auth paths. Keys
fingerprint identity fields only, and an undetectable identity is `unknown`.
Quota capture checks identity before and after a probe and refuses a changed
attribution. Probes run in a fresh 0700 directory under the host state root;
failing to create one is a host fault, not an identity change.
Version-probe refusals report termination, timeout and stdout truncation, or
identify empty output, without quoting captured contents.
- Why (default): misattribution would spend or expose the wrong account; a transient
  version-probe failure needs an actionable diagnostic without output disclosure.
- Test: `crates/providers/src/quota_live/claude.rs::fixed_directory_rejects_changed_account_fingerprint`;
  `crates/providers/src/host.rs::start_up_sweeps_crashed_probes_but_never_a_live_one`;
  `crates/cli/src/run/provider/tests_provider_auth_setup.rs::version_probe_reports_outcomes_without_disclosing_output`
- Owner: `crates/providers/src/identity.rs`, `crates/providers/src/host.rs`,
  `crates/cli/src/run/provider/`

### Pins filter, credentials stay put

An account pin filters eligibility; it never switches accounts when quota runs out.
Credentials are never copied between identity directories. Quota
credentials stay in memory and are never logged, written, put on argv or
passed in a child environment; linked credential files are rejected, and usage calls pass the
header to `curl --config -` on stdin.
- Why (default): the user decides which account pays, and a probe must not leak.
- Test: `crates/routing/src/tests/availability.rs::account_override_selects_a_pool_member_and_keeps_other_provider_fallbacks`;
  `tests/quota.rs::disabled_modes_never_read_auth_or_invoke_external_tools`.
  No test asserts tokens are absent from curl argv and environment, or that
  credentials are never copied between directories (gaps).
- Owner: `crates/routing/src/`, `crates/providers/src/quota_live.rs`

### Credential reads record authority first

A quota credential read records the actor, the exact credential source, the
purpose and the expiry before it reads, and retains only the non-secret
authority reference.
- Why (default): a read of a credential must be attributable without keeping the secret.
- Test: none (gap); `CredentialAuthority` covers lane dispatch only.
- Owner: `crates/providers/src/quota_live.rs`

## Quota

### Unknown is not exhausted; refusals are scoped

Unknown or stale quota never causes a refusal. Refusals are model- and
account-scoped unless wire evidence proves a wider pool, and independent
pools never exhaust each other. A newer comparable observation or the reset
clears only that refusal. For adapters that group quota into pools, a legacy
refusal without a stored pool is compared against the model family's pool,
derived from the model prefix; other adapters keep the bounded refusal wait.
An account whose latest reading is exhausted is
skipped for an available account whatever its pacing policy: hard when the reading
is fresh, by rank when it is stale. Only a quota-limited outcome cools an account;
auth failures never switch accounts. Provider capacity errors settle as
`model_unavailable` with the provider's reason visible in lanes and record a
separate, advisory capacity cooldown on the host.

For an identified account, uncertain availability is a selection preference:
current evidence ranks ahead of stale or unknown evidence. When uncertainty is
selected, the lane records a warning and lets the provider answer. A missing
executable, identity mismatch, disclosure or model-policy denial, active
provider refusal and exhausted allowance remain exclusions. If every account
is excluded, the decision carries each account's state and remedy.

One quota function, `Admission::account_block`, assesses persisted account
mismatch and live provider refusals (including the free-pool refusal) with their
expiry and clearing rules. Resolution applies it to each otherwise eligible
account before choosing one, so a blocked account yields to another serving the
same model; blocks surface as the reason codes `account_mismatch`,
`free_tier_refused` and `provider_refused`. Admission reuses the same function on
freshly reloaded records immediately before launch, so a refusal written after
resolution still prevents it. Invalid evidence excludes only the account it
belongs to (`invalid_account_evidence`) and fails admission; absent evidence is
not proof of readiness. Cooldown stays a selection preference, never a block.
A provider's plan usage limit is the one refusal that spans models. When a lane
ends on it (Codex's `You've hit your usage limit.`, either as the stream error
or the failed turn; `codex_error_info: usage_limit_exceeded` without that phrase
stays model-scoped), the host records a refusal on that account whose pool is the account route `*`, with the reset the
provider names (`try again at …` in local time, or `try again in …`) or the
one-hour wait when it names none. `account_block` holds every model on that
account until the reset, or until a newer reading shows headroom in every window
the account reports. `provenant quota` shows the account `exhausted` with that
reset, resolution and pools pass to another account or member, and a graph node
falls back as for any `provider_refused` exclusion. A limit that names a model
(`usage limit for <model>`) stays scoped to that model. The lane itself settles
`quota_limited`; its reason names the account and the reset, so several lanes
stopped by one spent account read as one cause. Only the limit and its reset are
read from the provider's text. `credit_only` means credits are spendable: a
spent window reads `exhausted` when the provider reports no balance, reports
`hasCredits: false`, or reports that spending is blocked.
- Why (default): one probe outage or model limit must not take a provider offline.
- Why (account limit): one spent account must not take pool and exact-route
  lanes down while other accounts have capacity (#3109).
- Test: `crates/quota/src/admission.rs::unknown_and_stale_do_not_manufacture_exhaustion`;
  `crates/quota/src/admission.rs::admission_tests::account_usage_limit_holds_every_model_until_its_reset`;
  `crates/quota/src/schedule.rs::zen_tests::account_usage_limit_reads_exhausted_until_its_reset`;
  `crates/providers/src/codex.rs::usage_limit_tests::a_plan_usage_limit_holds_the_account_until_its_named_reset`;
  `crates/providers/src/codex.rs::usage_limit_tests::a_model_usage_limit_holds_only_that_model`;
  `crates/providers/src/tests_stream_outcomes.rs::codex_usage_limit_text_without_error_info_is_quota_limited`;
  `crates/providers/src/quota/tests.rs::codex_spent_window_without_spendable_credits_is_exhausted_not_credit_only`;
  `crates/routing/src/tests/exclusions.rs::account_usage_limit_excludes_the_spent_account_for_every_model`;
  `crates/cli/src/run/holder.rs::tests::an_account_usage_limit_settles_typed_and_holds_every_model_until_reset`;
  `crates/routing/src/tests/alternatives.rs::stale_availability_is_selectable_but_current_account_wins`;
  `crates/quota/src/admission.rs::independent_pool_reset_and_refusal_scope`;
  `tests/graph_provider.rs::codex_capacity_settles_without_quota_refusal`;
  `crates/routing/src/tests/policies.rs::spent_account_is_skipped_for_an_available_one_even_when_its_reading_is_stale`;
  `crates/quota/src/admission.rs::admission_tests::account_block_types_mismatch_free_and_scoped_refusals`;
  `crates/routing/src/tests/exclusions.rs::persisted_mismatch_excludes_account_and_selects_another`;
  `crates/routing/src/tests/exclusions.rs::invalid_account_evidence_excludes_only_that_account`;
  `crates/routing/src/tests/exclusions.rs::free_pool_refusal_excludes_the_account_for_every_free_model`;
  `crates/routing/src/tests/exclusions.rs::live_scoped_refusal_excludes_only_the_matching_account_and_model`;
  `crates/cli/src/route/tests_2.rs::refused_account_falls_through_to_another_for_the_same_model`;
  `crates/cli/src/route/tests_3.rs::review_substitution_skips_refused_unused_families`;
  `crates/records/tests/forward_compatibility.rs::route_exclusion_reason_codes_round_trip`
- Owner: `crates/quota/src/admission.rs`

### Revisioned, bounded refresh

A quota write re-reads the revision under a bounded lock and replaces the record atomically.
A refresh reserves a claim, releases the lock, runs, then commits only
against the matching revision, so stale completions are discarded. A tick never
sleeps, runs at most three probes, each with a deadline and a 1 MiB bound, within a
55 s budget, and config cannot raise those limits.
- Why (default): the menu bar ticks unattended and concurrently.
- Test: `crates/quota/src/schedule.rs::competing_claims_and_stale_completions_cannot_overwrite`;
  `tests/quota.rs::configuration_cannot_raise_probe_limits_and_publication_failure_is_io`
- Owner: `crates/quota/src/schedule.rs`

## Nested lanes, MCP and hooks

Every lane except Agy's can use Fabric and the utility models through a
generated MCP entry; the user's hook and MCP configuration is excluded.

### Lane identity and refusals

Each lane's provider environment and MCP entry pin
`PROVENANT_LANE_DEPTH`, `PROVENANT_PARENT_ATTEMPT_ID`,
`PROVENANT_RUN_STATE_DIR` and `PROVENANT_AGENT_LABEL=lane-<AttemptId>`.
Review lanes dispatched with a head also receive `PROVENANT_LANE_HEAD` and
`PROVENANT_LANE_BASE`, containing the resolved commit SHAs.
In that environment Fabric refuses coordination mutations
(`lane_coordination_refused`) and local resume (`continuation_unsupported`);
graph submission, subtree stop and graph reads cross the attempt-bound host
relay. `status`, `output` and `output --trace` select the calling graph or its
children; journals and credentials stay on the host.
- Why (default): a lane must not move shared tasks, claims or leases.
- Test: `crates/providers/src/lane.rs::lane_mcp_entries_have_attempt_unique_agent_labels`
- Owner: `crates/providers/src/lane.rs`

### The host validates and launches children

`run` inside a lane submits a revisioned child patch through the host relay.
The host resolves the parent from its custody-bound attempt, journals admission
and launches through the ordinary graph owner. Children share the graph's
budget and cancellation; a waiting parent yields its concurrency slot.

Children may be writers with their own registered worktree or a checkout
provisioned by the host. A child's roots and capabilities need not be a subset
of the parent's. Selecting a repository with `--project` preserves the child's working
directory; a writer uses that repository's registered worktree and custody. Ordinary launch policy still refuses
credential roots, state roots and write roots overlapping the worktree. A
refusal carries a safe cause and a repair the caller can act on.

The host binds admission to the calling attempt and its requester rather than
trusting a lane-written record. A child can select a different adapter and
account for independent review, within the user's authority envelope.
- Why (default): cross-family delegation must not expose trusted host journals or make
  lane-controlled identity authoritative.
- Test: `src/fabric/interface.rs::tests::child_launch_crosses_the_lane_transport_and_provisions_writer_fanout`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::rich_child_attempt_controls_define_replay_identity_and_reach_the_launch`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::writer_children_get_their_own_worktree_and_depth_is_bounded`
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `src/fabric/owner.rs`

### Stopping children

Parent settlement closes attached children; detached subtrees remain independent.
A targeted child stop preserves its parent and siblings. Cancellation and its
pending effects survive checkpoint reload.
- Why (default): an attached child must not outlive completed work, while explicitly
  detached work retains its own lifecycle.
- Test: `crates/fabric-runtime/src/graph/relay_tests.rs::successful_parent_closes_attached_children_but_not_detached_children`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::stopping_selected_child_preserves_parent_and_sibling`;
  `crates/fabric-runtime/src/graph/relay_tests.rs::unstarted_child_cancellation_survives_checkpoint_reload`
- Owner: `crates/fabric-runtime/src/graph/relay.rs`

### Host-pinned Git and writer grants

Git is resolved on the host and pinned in provider and MCP environments as
the only Git lanes may execute. The pin keeps the stable locator (such as
`/opt/homebrew/bin/git`) and runs that. Its canonical target must be a
regular file named `git`, owned by the current user or root, and neither
group- nor world-writable. Every ancestor directory of the locator and the
target must not be world-writable without the sticky bit; a group-writable
ancestor owned by root or the current user is accepted (Intel Homebrew's
`/usr/local/bin`). A `stat` through the locator before each spawn, and an
ENOENT from a spawn, re-validate the target when it changed, so
`brew upgrade git` does not break a long-lived process while a replaced or
loosened target is refused. Writer-only grants come
from confinement's writer profile, and each attempt records its write
allowances and Git grants digest. Writers use a complete private Git directory
and index under attempt scratch with the shared object store as a read-only
alternate. The one wrapper source selects these paths, only for Git acting on
the lane checkout itself, and supplies publishing credentials. Shared refs,
config, hooks, info and linked admin metadata stay read-only throughout their
subtrees. The host imports the pinned result through Git receive quarantine
and publishes the real branch itself; lane configuration never becomes host
configuration.
- Why (default): a lane-chosen Git or helper would run with host authority.
- Test: `crates/os/src/git.rs::override_wins_and_invalid_override_does_not_fall_back`;
  `crates/confinement/src/utility.rs::executable_pin_keeps_the_locator_and_follows_a_swapped_target`;
  `crates/cli/src/run/provider/tests_retained_state_and_workspace.rs::host_write_paths_reach_writers_only`
- Owner: `crates/os/src/git.rs`

## Not provided

- Rotating pools, a selection store and inferred class or size routing.
- Native-seat inference, host avoidance and account pins on routes.
- A recursive swarm manager.

A route intent with image input prefers ready pool members whose adapter
advertises image support. The normal weights and zero-weight order apply within
that subset. If no ready member advertises support, selection is unchanged;
the preference never refuses an explicit model or creates authority.

A dispatch can require an adapter-owned skill location with `--require-skill-location`, the manifest task field `require_skill_location`, or the same field on a graph node. Preset runs apply the CLI requirement to their task nodes, including deferred review lenses. Pool selection and launch check that requirement against the adapter registry; the default is false. This capability does not grant access to a skill tree.
