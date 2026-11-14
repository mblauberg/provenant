# State and config

Owner: `crates/core` (IDs, roots, time), `crates/records` (codecs, digests),
`crates/config` (config, release, managed install)
Anchor: `crates/core/src/roots.rs`, `crates/records/tests/contracts.rs`,
`crates/config/src/managed_tests.rs`

Keys and defaults: `provenant config schema`. Operator steps: the
[install](../runbooks/install.md) and [GC](../runbooks/gc.md) runbooks.

## Roots and identities

### Root selection never falls back

An empty or relative explicit root fails and never yields to the default. A
root that overlaps a protected location is refused. Reads create no state.
The first 2.0 install stops 1.x agents and seeds only `config.toml`, `host-id` and `install/receipt.json` into an absent or empty default root; the 1.x root remains untouched.
- Why: a wrong root mixes or destroys another installation's state.
- Test: `crates/core/src/roots.rs::roots_refuse_home_checkout_old_roots_and_any_agent_run_component`;
  `tests/parser.rs::root_uses_release_location_and_explicit_state_without_creating_it`
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### Identifiers have one spelling

A value that can normalise two ways is rejected at the wire boundary.
- Why: two spellings let one record alias another.
- Test: `crates/core/src/ids.rs::identifiers_are_validated_at_the_wire_boundary`
- Owner: `crates/core/src/`

### Graph names retain one identity

A graph name identifies one graph generation for its project's lifetime.
Checkpoint collection leaves a durable name reservation because retained
attempt, chair and event records continue to refer to the graph by name. A
collected name is not allocated to a later graph.
- Why: reusing the name can collide with retained attempt keys or inherit a
  stale chair association.
- Test: `crates/fabric-runtime/src/graph/journal_tests.rs::checkpoint_collection_keeps_pending_graphs_and_reserves_collected_names`
- Owner: `crates/fabric-runtime/src/graph/journal.rs`,
  `src/fabric/interface.rs`

## Records

### Commit before signal

Owners commit durable state before the lossy change-generation signal; a
failed signal never fails the committed write. Readers treat an absent or reset
generation as unknown and re-read.
- Why: a hint must not become a second source of truth.
- Test: `crates/fabric-store/src/tests.rs::signal_failure_reports_a_committed_fabric_write`;
  `crates/cli/src/console.rs::changes_reports_unknown_and_comparison`
- Owner: `crates/os/src/changes.rs`

### Strict, bounded records

Unsupported versions fail before mutation. The reader rejects ambiguous or
unbounded input and never echoes the rejected value. Closed codecs reject or
discard unknown fields; records with explicit extension maps preserve their
extensions. Edge-built facts are `Serialize` only.
- Why: a forged file must not become evidence.
- Test: `crates/records/tests/contracts.rs::bounded_decode_fails_closed_for_duplicates_utf8_trailing_and_integer_overflow`;
  `crates/records/tests/contracts.rs::typed_decode_errors_do_not_disclose_rejected_values`
- Owner: `crates/records/src/`

### One digest preimage

`CanonicalV1` bytes are deterministic and domain-framed; it is not RFC 8785.
- Why: a digest is an identity only if every writer computes the same bytes.
- Test: `crates/records/tests/contracts.rs::canonical_v1_sorts_keys_uses_required_escapes_and_separates_digest_domains`
- Owner: `crates/records/src/canonical.rs`

## Configuration

### Config never widens authority

Syntax, version and filesystem-boundary failures refuse loading; an invalid
optional value warns and uses the default, and a malformed restriction never
becomes permission. `config set` publishes atomically and keeps unknown keys;
`config show` redacts secrets.
Commands load their declared domains; invalid unused domains warn, and effective defaults retain their compiled source.
One sourced port table declares verify, land, CI observation, worktree layout, naming, post-create, tracker, release and admission backends; repository values at `.provenant/ports.json` override host `state/ports.json`, then compiled builtin defaults. This repository stores its source at `config/ports.json` and links the optional Provenant path there; CI reads the source directly. Verify tiers declare free-form names, an explicit default, selection and host-load policy, while the builtin layout declares its roots and branch path once.
The land command reuses `records::landing::RepositoryCommand`: exit 0 means landed, other nonzero exits refuse except its declared unknown code. A command port inherits only the declared execution-context environment (paths, home, temporary directory, locale and toolchain selection), plus explicit owner-supplied values; ambient credential and agent variables are not forwarded. A command port runs configured argv directly, followed by owner-supplied arguments, with no shell; declared exit codes select succeeded, unknown or refused, and incomplete capture is unavailable. `none` disables that port without selecting the builtin behavior.
Naming commands receive the requested branch and project path as arguments; complete single-line branch output derives a name. Empty output keeps the request; invalid output or a command failure warns and keeps it. Post-create commands receive the branch and new worktree path. Admission commands receive `PROVENANT_PORT_PHASE=acquire|release`, `PROVENANT_ADMISSION_ATTEMPT`, `PROVENANT_ADMISSION_EPOCH`, `PROVENANT_ADMISSION_WORKTREE`, `PROVENANT_ADMISSION_PROJECT` and an informational `PROVENANT_ADMISSION_OWNER_PID`. The repository owns its semaphore keyed by attempt and epoch: acquire and release are idempotent, and release fences any late acquire for the same epoch. A queued retry uses a new epoch; Provenant seals the exact command/source before acquisition and invokes release outside terminal transaction locks. Busy acquisition skips the queued attempt for retry, without a Provenant semaphore.
CI observation commands receive `PROVENANT_PORT_REQUEST_PATH` and `PROVENANT_PORT_IF_NONE_MATCH` (empty when no validator is sent), and emit `{"status": number, "headers": {"etag": "..."}, "body": "UTF-8 response bytes"}`; 304 responses use an empty body. Repository discovery uses the same selected backend with the literal request path `repos/{owner}/{repo}`; its body is the UTF-8 JSON object `{"full_name":"owner/repo"}` and `none` refuses the lookup without using the builtin adapter. Verify command ports substitute `{tier}` in their declared argv and receive `PROVENANT_VERIFY_TIER`, `PROVENANT_VERIFY_PROJECT_ROOT` and the private `PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON`, which contains only the resolved verify declaration. The builtin engine uses that declaration so host overrides apply; direct repository CI still reads `config/ports.json` without Provenant. Tracker commands receive one issue number and emit the normalized projection `{body, state, kind}`. Release commands receive optional `--base` and `--head`, then emit the complete `{findings: [{rule, path, line, message}]}` set for Provenant to page; `path` uses the existing `PathWire` representation and `line` is one-based. Command receipts under host state bind typed owner/backend, command digest, source, phase and hashed request key to outcome, exit code, elapsed time and stdout digest; these receipt bodies omit argv, environment and output contents.
- Why: errors need visible remedies without expanding authority.
- Test: `crates/config/src/tests.rs::config_warns_on_optional_input_but_refuses_syntax_and_versions_without_echoing_input`;
  `crates/config/src/tests.rs::knob_edit_preserves_unknown_keys_while_applying_the_edit`
- Owner: `crates/config/src/`

### Retention keeps evidence

GC retains evidence and unimported Git work; it expires resumable provider state
only after archiving history. It skips busy items, protects active reservations
and continuations, and uses identity-checked deletion without following links.
Usage retention belongs to [telemetry](telemetry.md#the-usage-ledger-outlives-the-run-store).
Each retention class declares its owner, trigger, eligibility and settings; GC reports bytes per class with complete, partial or unavailable measurements.
Removal records preserve subject, tip, subsumed or landed predicate and evidence source; shallow history, conflicted merge trees, offline forge and rewritten default branches remain typed kept outcomes.
- Why: freeing space must not destroy evidence or a session in use.
- Test: `crates/run/src/retention.rs::gc_spares_nonterminal_attempts_and_contention_and_reservations`;
  `crates/run/src/retention.rs::gc_retries_only_authorised_tombstones_and_never_follows_links`
- Test: `crates/os/src/fs/ledger.rs::tests`;
  `crates/telemetry/src/ledger/tests.rs::concurrent_retries_count_an_attempt_once`;
  `crates/run/src/retention.rs::age_collection_keeps_unimported_git_until_custody_is_proven`;
  `crates/run/src/target_seed_gc.rs::tests::active_clone_lock_prevents_collection`
- Owner: `crates/run/src/retention.rs`

## Release identity

### The source digest is the binary's identity

It covers shipped source, lockfile, toolchain, packager and bundled assets, with
no override; test-only edits do not change it.
- Why: the installed binary must be provably the reviewed source.
- Test: `crates/records/src/source.rs::hashes_sorted_framed_names_and_exact_contents`;
  `build.rs::member_test_edits_do_not_change_the_source_digest`
- Owner: `build.rs`, `crates/records/src/source.rs`

### Releases are immutable

Install verifies and smoke-tests a release before trusting it. `release.json`
is published last and a published release is never repaired. A digest proves
identity, not publisher trust.
- Why: a partial or swapped release must never run.
- Test: `crates/config/src/managed_tests.rs::bytes_and_modes_are_verified_and_existing_release_is_immutable`;
  `crates/config/src/managed_tests.rs::interrupted_release_publish_leaves_no_manifest_and_reapplies_with_real_staging_file`
- Owner: `crates/config/src/release.rs`

## Managed install

### Preview before every write

A failed preview prevents writes, and `--dry-run` creates no root, lock or
receipt. Identity and bytes are rechecked before each publication; drift stops
the install.
- Why: the user's harness config changes only in view.
- Test: `crates/config/src/managed_tests.rs::dry_run_and_broken_preview_create_nothing`;
  `crates/config/src/managed_tests.rs::drift_between_preview_and_apply_refuses_before_file_writes`
- Owner: `crates/config/src/managed.rs`

### Only proven-owned entries change

Removal unlinks only entries whose value still matches the receipt, never
recursively; adoption does not authorise deletion. After a home or
state-root move, stale entries are reported without their paths being inspected.
- Why: an old path may now belong to something else.
- Test: `crates/config/src/managed_tests.rs::uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary`;
  `crates/config/src/managed_tests.rs::uninstall_after_home_rename_never_acts_on_old_paths`
- Owner: `crates/config/src/managed.rs`

### Foreign bytes and user hooks survive

Edits replace only owned values, keeping comments and order; malformed or
symlinked configs are refused unchanged. Edited hooks are not overwritten.
- Why: a harness config holds the user's other tools and secrets.
- Test: `crates/config/src/managed_tests.rs::five_client_install_preserves_foreign_hooks_comments_and_reapplies`;
  `crates/config/src/managed_tests.rs::edited_inherited_hook_is_relinquished_but_explicit_on_names_recovery`
- Owner: `crates/config/src/splice.rs`, `crates/config/src/hooks.rs`

### LaunchAgents are owned like any entry

The peer-agent and tick plists join the preview and receipt. A symlink at the
plist or a parent is refused, and an older plist is adopted only on an exact
byte match.
- Why: a LaunchAgent runs unattended with the user's authority.
- Test: `crates/config/src/managed_tests.rs::peer_agent_refuses_symlinked_plist_and_parent_before_writes`;
  `crates/fabric-runtime/src/peer_agent.rs::provider_path_skips_a_registered_project_and_clones_nested_in_it`
- Owner: `crates/config/src/managed.rs`, `src/tick_agent.rs`

### Re-apply is the recovery

A partial install lists applied and remaining actions, and rerunning `install`
recovers it, even with no receipt. There are no rollback journals.
- Why: one idempotent path is easier to trust than a rollback engine.
- Test: `crates/config/src/managed_tests.rs::crash_after_any_publication_reapplies_without_a_receipt`;
  `crates/config/src/managed_tests.rs::receipt_publish_failure_reports_partial_actions_and_reapplies`
- Owner: `crates/config/src/managed.rs`
