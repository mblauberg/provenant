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

### Claude token refresh

The sandbox never refreshes or writes the keychain. The host refreshes a
token with under 15 minutes left, using one unconfined CLI run with every
customisation off, in a fresh private directory removed afterwards, and
never copies credentials between identities. Shell-capable Claude lanes
scrub child environments and deny keychain reads. Claude still holds the
token and has outbound HTTPS, so isolation is incomplete.
- Why: credential writes belong to the user's own sign-in flow.
- Test: `crates/providers/src/tests.rs::claude_login_refreshes_only_near_expiry_with_customisations_off_and_never_prints_the_token`;
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
rate limits are short per-model waits and auth failures never switch
accounts.
- Why: one probe outage or model limit must not take a provider offline.
- Test: `crates/quota/src/admission.rs::unknown_and_stale_do_not_manufacture_exhaustion`;
  `::independent_pool_reset_and_refusal_scope`
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
- Why: a writer child could leave scripts that run unconfined, and child
  storage is lane-writable, so only the host's sealed view counts.
- Test: `crates/run/src/dispatch_tests.rs::host_refuses_writer_child_after_request_rewrite`;
  `::host_refuses_child_access_beyond_its_parent_task`
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
