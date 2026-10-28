# Routing, providers and quota

Owner: `crates/routing` (catalogue, resolve), `crates/providers` (adapters,
accounts, decoding), `crates/quota` (windows, refusals, scheduling)
Anchor: `crates/routing/src/tests.rs`, `crates/providers/src/tests.rs`,
`crates/quota/src/admission.rs`, `crates/run/src/dispatch_tests.rs`

This spec owns how a dispatch becomes one exact provider model, how adapters
build and decode launches, and how quota decides readiness. Run owns
admission and publication ([run](run.md)); supervisor and confinement own
custody and grants ([supervisor](supervisor.md)). Grammar is in the
generated [CLI reference](../reference/cli.md), keys in the [config
reference](../reference/config-keys.md), and bundled pools in
`provenant route list`. Paths are relative to `tools/front-door/`.

## Resolution

### Exact selection, seeded pools

A request names exactly one `--model ADAPTER/ID` or `--route NAME`. No
adapter, upgrade, effort default or downgrade is inferred, and an unknown
explicit ID runs only if availability, disclosure and confinement are
independently established. A pool draws among ready members, weighted by
the provider's pacing factor (0.25 to 2.0) and seeded from the operation ID
and task index; the receipt records each weight, the random roll and the chosen member.
- Why: retries pick the same model without stored state, and the receipt
  names what ran.
- Test: `crates/routing/src/tests.rs::exact_unknown_is_retained_but_never_upgraded_or_activated`;
  `::table_driven_weighted_pools_record_effort_readiness_roll_and_seed`
- Owner: `crates/routing/src/`

### Capacity cooldown

A host records a short-lived capacity cooldown for the affected model and account
when a provider reports capacity. Pool-backed routes and senior/junior draws skip
live cooldowns; if all candidates are cooling, the normal choice remains available
and the receipt explains the fallback. Explicit model dispatches and resumes ignore
capacity cooldowns. Quota refusals remain separate and continue to apply. The host
setting `domains.routing.capacity_cooldown_seconds` defaults to 600 seconds; zero disables new cooldowns.
`quota` and `lanes` show live cooldowns and their remaining time.
- Test: `crates/quota/src/schedule.rs::tests::reserve_adopts_a_renamed_label_for_the_same_account_key`;
  `crates/quota/src/schedule.rs::tests::report_shows_only_live_capacity_cooldowns`;
  `crates/cli/src/run.rs::tests::lanes_show_only_live_capacity_cooldowns`;
  `tests/dispatch_provider.rs::capacity_holder_persists_and_explicit_and_resume_ignore_cooldown`

### One resolution, bounded alternatives

Resolution runs once against immutable catalogue, availability and quota
snapshots and returns one route with at most two ordered alternatives, each
passing the same checks; Run never rebuilds candidates. Fallback is
read-only and opt-in. Unsupported effort fails; where clamping is permitted, the requested,
applied and observed values are recorded. `route list` and `resolve` start no
process and mutate nothing.
- Why: the fallback plan is decided once and visible.
- Test: `crates/routing/src/tests.rs::alternatives_are_bounded_distinct_and_keep_the_callers_order`;
  `tests/route.rs::help_list_and_resolve_are_pure_and_resolve_reports_exact_candidates`
- Owner: `crates/routing/src/`

### Go monthly allowance floor

Every OpenCode Go candidate, including an explicit or uncatalogued exact ID,
must have a recorded monthly allowance at least equal to
`domains.routing.opencode_go.min_monthly_allowance_usd`. The default is $60;
zero disables the check. Unknown allowances are refused when the floor is set.
Refusals identify the model, allowance or unknown status, and the setting.
Zen is a separate account and is unaffected. The floor is independent of
quota calibration and pacing.
- Why: protect the shared Go plan from low-allowance models regardless of
  selection path.
- Tests: `crates/routing/src/catalog.rs::go_allowance_tests::go_allowance_floor_handles_limits_unknown_and_zen`,
  `crates/cli/src/route.rs::route_tests::go_floor_gates_explicit_pool_and_fallback_dispatch`
- Owner: `crates/routing/src/`

### Go pool quota coverage

OpenCode Go local ledger usage is labelled `source: local-ledger` and
`coverage: partial`; it is a lower bound because other devices are absent.
An active per-account calibration supplies dashboard percentage floors,
applied as the maximum of ledger and floor at each read. Expiry restores raw
ledger usage. Missing or expired floors do not exclude pool members; route list
and resolve identify uncalibrated windows so users can tighten pacing.
Exhaustion, authentication and quota limits still exclude members.
Configuration and the operational limits are
in the [calibration runbook](../runbooks/configuration.md#opencode-go-quota-calibration).
- Why: Go models remain available while partial coverage is visible.
- Test: `crates/quota/src/calibration.rs::tests::calibration_takes_maximum_and_expires_even_with_cached_ledger`
- Owner: `crates/quota/src/calibration.rs`, `crates/routing/src/resolve.rs`

### Confidentiality

A confidential request admits only models whose catalogue row says they do
not train on prompts; the test is training, not price. Free-tier models need
`--allow-free`. Fast or priority service is never enabled.
- Why: prompts carry private code; cost is not a privacy signal.
- Test: `crates/routing/src/tests.rs::free_pool_members_require_opt_in_and_confidential_models_are_excluded`
- Owner: `crates/routing/src/`

### Families come from evidence

A model family is established only by provider protocol evidence verified
against the catalogue revision, never inferred from spelling, a broker name or the
requested ID. Decoders read documented event positions and never search
tool or subagent output for the answering identity.
- Why: cross-family review is an assurance claim and must not be forgeable
  by output text.
- Test: `crates/routing/src/tests.rs::family_requires_provider_answer_evidence_at_the_same_revision`;
  `crates/providers/src/tests.rs::synthetic_and_api_error_messages_cannot_change_answering_identity`
- Owner: `crates/providers/src/`

### Closed adapter table

`records::ADAPTER_SPECS` is the closed adapter table that config, CLI and
`providers::REGISTRY` share. Dispatch probes only the adapters that its route, pool
and fallbacks name. Tier preference is guidance owned by the orchestrate
skill; it never waives authority, confinement or assurance gates.
- Why: one closed list keeps every surface agreeing; routing grants no
  authority.
- Test: `crates/cli/src/install.rs::adapter_specs_cover_config_routing_and_cli_names`;
  `crates/routing/src/tests.rs::probe_adapters_include_only_requested_routes_pools_and_fallbacks`
- Owner: `crates/records/src/adapters.rs`

## Providers

### Answer and exit are separate facts

Providers never signal a process or publish completion. Structured errors
outrank claimed success, provider prose about quota asserts nothing, and
malformed or oversized frames fail closed. Session continuation binds the exact
adapter, session, account, host, model and controls.
- Why: a provider's text must not decide an outcome or switch identity.
- Test: `crates/providers/src/tests.rs::terminal_answer_and_nonzero_exit_remain_separate_facts`;
  `::continuation_preserves_session_and_rejects_changed_identity_and_grants`
- Owner: `crates/providers/src/`

### Launch specs and adapter isolation

`providers::prepare` builds the exact argv, environment and profile in
memory; only a redacted summary persists, and there is no raw caller argv.
Each adapter is subordinate to the outer profile: Codex's own sandbox is
off; Claude gets its token on a descriptor closed before spawn, never in the
environment; Agy's private home links only the login keychain; Cursor
executes only its resolved version; OpenCode denies shell and web fetch.
Exact argv is pinned by `tests/fixtures/providers/<adapter>/recordings.json`.
- Why: the launch spec carries credentials and authority.
- Test: `crates/providers/src/tests.rs::selected_accounts_control_provider_environment_and_exact_authentication_grants`;
  `::agy_runs_in_a_private_home_that_links_only_the_login_keychain`
- Owner: `crates/providers/src/`

### Agy read-only commands and denials

Agy auto-approves tools in headless read-only and writer lanes. The outer
confinement profile is the boundary: read-only lanes may execute system
programs from `/bin`, `/usr/bin` and `/usr/libexec`, with PATH restricted to
`/usr/bin:/bin` and global and system Git configuration disabled. Their only
write grants remain attempt scratch and private state. Like read-only Codex
lanes, read-only Agy lanes can run system programs with the lane's existing
network access, so read roots must not hold anything the lane may not see.
On a successful Agy result, a denial with an empty response fails the lane as
`provider denied: <name>`; a non-empty response succeeds and records the denial
as a non-fatal note. Denials do not override an earlier failure.
- Why: headless permission prompts otherwise deny read-only commands, and a
  remaining denial must not be mistaken for an empty response.
- Test: `crates/providers/src/tests.rs::spilled_prompts_reach_argv_adapters_with_file_read_permissions`;
  `::agy_writers_exec_what_codex_writers_exec`;
  `crates/providers/src/seatbelt_snapshots.rs::agy_read_only_compiled_profile_denies_read_root_write`;
  `tests/dispatch_provider.rs::agy_denial_result_reaches_terminal_reason`
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
- Why: credential writes belong to the user's own sign-in flow.
- Test: `crates/providers/src/tests.rs::claude_login_refreshes_only_near_expiry_with_customisations_off_and_never_prints_the_token`;
  `::claude_concurrent_host_refreshes_do_not_reuse_a_rotating_token`;
  `::claude_writer_shell_has_private_config_scrubbed_children_and_rust_tools`
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
- Why: misattribution would spend or expose the wrong account; a transient
  version-probe failure needs an actionable diagnostic without output disclosure.
- Test: `crates/providers/src/quota_live/claude.rs::fixed_directory_rejects_changed_account_fingerprint`;
  `crates/providers/src/host.rs::start_up_sweeps_crashed_probes_but_never_a_live_one`;
  `crates/cli/src/run/provider.rs::version_probe_reports_outcomes_without_disclosing_output`
- Owner: `crates/providers/src/identity.rs`, `crates/providers/src/host.rs`,
  `crates/cli/src/run/provider.rs`

### Pins filter, credentials stay put

An account pin filters eligibility; it never switches accounts when quota runs out.
Credentials are never copied between identity directories. Quota
credentials stay in memory and are never logged, written, put on argv or
passed in a child environment; linked credential files are rejected, and usage calls pass the
header to `curl --config -` on stdin.
- Why: the user decides which account pays, and a probe must not leak.
- Test: `crates/routing/src/tests.rs::account_override_selects_a_pool_member_and_keeps_other_provider_fallbacks`;
  `tests/quota.rs::disabled_modes_never_read_auth_or_invoke_external_tools`.
  No test asserts tokens are absent from curl argv and environment, or that
  credentials are never copied between directories (gaps).
- Owner: `crates/routing/src/`, `crates/providers/src/quota_live.rs`

### Credential reads record authority first

A quota credential read records the actor, the exact credential source, the
purpose and the expiry before it reads, and retains only the non-secret
authority reference.
- Why: a read of a credential must be attributable without keeping the secret.
- Test: none (gap); `CredentialAuthority` covers lane dispatch only.
- Owner: `crates/providers/src/quota_live.rs`

## Quota

### Unknown is not exhausted; refusals are scoped

Unknown or stale quota never causes a refusal. Refusals are model- and
account-scoped unless wire evidence proves a wider pool, and independent
pools never exhaust each other. A newer comparable observation or the reset
clears only that refusal. Only a quota-limited outcome cools an account;
auth failures never switch accounts. Provider capacity errors settle as
`model_unavailable` with the provider's reason visible in lanes and record a
separate, advisory capacity cooldown on the host.
- Why: one probe outage or model limit must not take a provider offline.
- Test: `crates/quota/src/admission.rs::unknown_and_stale_do_not_manufacture_exhaustion`;
  `::independent_pool_reset_and_refusal_scope`;
  `tests/dispatch_provider.rs::codex_capacity_settles_without_quota_refusal`
- Owner: `crates/quota/src/admission.rs`

### Revisioned, bounded refresh

A quota write re-reads the revision under a bounded lock and replaces the record atomically.
A refresh reserves a claim, releases the lock, runs, then commits only
against the matching revision, so stale completions are discarded. A tick never
sleeps, runs at most three probes, each with a deadline and a 1 MiB bound, within a
55 s budget, and config cannot raise those limits.
- Why: the menu bar ticks unattended and concurrently.
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
reads, and submit, cancel and output on the child store, remain.
- Why: a lane must not move shared tasks, claims or leases.
- Test: `tests/fabric_transport.rs::lane_mcp_refuses_coordination_but_allows_reads_and_child_submission`;
  `crates/providers/src/lane.rs::lane_mcp_entries_have_attempt_unique_agent_labels`
- Owner: `crates/providers/src/lane.rs`

### Child stores

Each attempt's child run store is `RUN_STATE/children/<AttemptId>/`; the
launch grants only that subtree, never `STATE/runs/` or the parent
database. Submission refuses a child above `lanes.max_depth` (default 2); the limit
bounds recursion and grants no authority.
- Why: a lane must not read or forge other runs' records.
- Test: `src/worktree_tests.rs::lane_prepare_isolates_nested_state`;
  `crates/run/src/dispatch_tests.rs::nested_lane_depth_stops_at_the_configured_maximum`
- Owner: `crates/run/src/plan.rs`

### The host validates and launches children

A lane's submission only seals queued child records; the host validates and
launches them. No lane may propose a writer or delivering child. A child's
network and roots must stay within the parent's, and it cannot continue an
attempt. The host launches only the request it validated, refuses one whose
digest changed, and launches at most one attempt per child task.
The digest and derived IDs identify the canonical request submitted by the
requester. The host applies inherited network restrictions as launch policy,
validates the effective access against the parent, and checks the original
digest again before launch. Every attempt's effective network is sealed in
its holder context, so grandchildren inherit the bound. Older live contexts
without this field default to provider-only network. A continuation in a child
store is refused with `CONTINUATION_UNSUPPORTED` because the prior host-sealed
network cannot be recovered reliably; dispatch a new child instead.
- Why: a writer child could leave scripts that run unconfined, and child
  storage is lane-writable, so only the host's sealed view counts.
- Test: `crates/run/src/dispatch_tests.rs::host_refuses_writer_child_after_request_rewrite`;
  `::host_refuses_child_access_beyond_its_parent_task`;
  `::host_launches_only_the_validated_child_request`;
  `::lane_store_refuses_continuation_without_sealed_network`;
  `crates/cli/src/run/children.rs::tests::restricted_parent_launches_child_with_inherited_chair`;
  `::inherited_network_survives_holder_context_for_grandchildren`;
  `::launch_seal_records_root_network_and_legacy_defaults_restrictively`
- Owner: `crates/cli/src/run/children.rs`, `crates/run/src/admission.rs`

### Children run as the parent's requester

A child launches for its parent's requester, sealed in the host-built holder
context, never the actor its record names. The host refuses an attempt it
launched once its record names another owner. A child may name another
adapter or account and gets that adapter's login grant. This is intended, for
cross-family review; the host limits reach, not credentials.
- Why: a lane must not launch work under someone else's authority.
- Test: `crates/run/src/dispatch_tests.rs::child_launches_for_its_parent_requester_not_its_store_record`;
  `::host_view_refuses_a_launched_child_renamed_after_start`
- Owner: `crates/run/src/admission.rs`

### Stopping children

A parent cancellation, deadline or signal stops only that worker's children,
through the directory handle, scope and requester held at hand-off and never
the child's record. Normal completion leaves children running. At most 64
handles are held, a lock probe on the captured inode frees one, and the
final scan refuses still-queued children. The stop is written to a lane-writable
directory, so a lane can undo it (an accepted residual risk).
- Why: renaming a directory must not redirect a stop, and descriptors stay
  below macOS's soft limit.
- Test: `crates/run/src/dispatch_tests.rs::host_stops_a_launched_child_from_held_scope_not_its_records`;
  `crates/cli/src/run/children.rs::final_scan_refuses_deferred_children_after_normal_parent_completion`
- Owner: `crates/cli/src/run/children.rs`

### Host-pinned Git and writer grants

Git is resolved once on the host, canonicalised and pinned in provider and
MCP environments as the only Git lanes may execute. Writer-only grants come
from confinement's writer profile, and each attempt records its write
allowances and Git grants digest. Remote-tracking refs and shared config
stay read-only.
- Why: a lane-chosen Git or helper would run with host authority.
- Test: `crates/os/src/git.rs::override_wins_and_invalid_override_does_not_fall_back`;
  `crates/cli/src/run/provider.rs::host_write_paths_reach_writers_only`
- Owner: `crates/os/src/git.rs`

## Not provided

- Rotating pools, a selection store and inferred class or size routing.
- Native-seat inference, host avoidance and account pins on routes.
- Concurrent writer batches or a recursive swarm manager.
