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
root equal to home or `/`, inside the checkout or a pre-1.0 root, or containing
`.agent-run` is refused. Reads create no state.
- Why: a wrong root mixes or destroys another installation's state.
- Test: `crates/core/src/roots.rs::roots_refuse_home_checkout_old_roots_and_any_agent_run_component`;
  `tests/parser.rs::root_uses_release_location_and_explicit_state_without_creating_it`
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### Identifiers have one spelling

A value that can normalise two ways is rejected at the wire boundary.
- Why: two spellings let one record alias another.
- Test: `crates/core/src/ids.rs::identifiers_are_validated_at_the_wire_boundary`
- Owner: `crates/core/src/`

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
unbounded input and never echoes the rejected value. Unknown fields survive round
trips. Edge-built facts are `Serialize` only.
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
- Why: errors need visible remedies without expanding authority.
- Test: `crates/config/src/tests.rs::config_warns_on_optional_input_but_refuses_syntax_and_versions_without_echoing_input`;
  `crates/config/src/tests.rs::knob_edit_preserves_unknown_keys_while_applying_the_edit`
- Owner: `crates/config/src/`

### Retention keeps evidence

GC expires resumable provider state only after archiving history, and leaves
results, captures, exit evidence and digests; resume then fails
`PROVIDER_STATE_EXPIRED` before spending. It never waits for locks, spares
reservations and pending continuations, deletes through identity-checked
tombstones, never follows links and writes nothing on a dry run
([ADR 0031](../adr/0031-retain-provider-history-and-expire-resume-state.md)).
- Why: freeing space must not destroy evidence or a session in use.
- Test: `crates/run/src/retention.rs::gc_spares_nonterminal_attempts_and_contention_and_reservations`;
  `crates/run/src/retention.rs::gc_retries_only_authorised_tombstones_and_never_follows_links`
- Owner: `crates/run/src/retention.rs`

### A format break needs acknowledgement

An install never opens an older-format store. Without `--reset-state` it stops
with `STATE_FORMAT_BREAK` and changes nothing, even with `--yes`; a held lane
custody lock blocks it regardless. The reset removes only the dropped run
state; ledgers, worktrees and branches stay.
- Why: deleting state is irreversible and a lane may be running.
- Test: `tests/install.rs::format_one_state_is_reset_only_on_acknowledgement`
- Owner: `src/state_reset.rs`, `src/install.rs`, `install.sh`

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
recursively. `--adopt-existing` never authorises deletion. After a home or
state-root move, stale entries are reported without their paths being inspected.
- Why: an old path may now belong to something else.
- Test: `crates/config/src/managed_tests.rs::uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary`;
  `crates/config/src/managed_tests.rs::uninstall_after_home_rename_never_acts_on_old_paths`
- Owner: `crates/config/src/managed.rs`

### Foreign bytes and user hooks survive

Edits replace only owned values, keeping comments and order; malformed or
symlinked configs are refused unchanged. Omitting `--hooks` refreshes unchanged
owned hooks and releases edited ones; `--hooks on` refuses an edited hook.
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
